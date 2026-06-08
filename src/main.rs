use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod adapters;
mod ats;
mod crawlers;
mod db;
mod discover;
mod http;
mod search;

use crate::db::{Db, StatusFilter};

#[derive(Parser, Debug)]
#[command(name = "ajs", about = "Automated job searches")]
struct Cli {
    /// Path to the SQLite database. Defaults to <repo>/jobs.db, baked at build time.
    #[arg(long, global = true, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/jobs.db"))]
    db: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a crawler against an aggregator site. Use "all" to run every registered crawler.
    Crawl {
        /// Crawler name, or "all"
        name: String,
    },
    /// Refresh job lists from ATS JSON APIs for known company slugs.
    /// Iterates over every company in the DB matching the given ATS kind.
    /// Use "all" to run every registered adapter.
    Sync {
        /// ATS name (greenhouse | ashby | lever | smartrecruiters | bamboohr |
        /// recruitee | workday), or "all"
        name: String,
    },
    /// Discover new ATS company slugs via web-search engine queries.
    /// Each ATS has a curated set of `site:` / phrase queries; classified
    /// hits become new `companies` rows that `sync` will then pull jobs for.
    ///
    /// Choose the engine with `--engine`:
    ///   brave     — needs BRAVE_API_KEY                          (default)
    ///   google    — needs GOOGLE_API_KEY + GOOGLE_CSE_ID         (Programmable Search Engine)
    ///   serper    — needs SERPER_API_KEY                         (Google via serper.dev)
    ///   tavily    — needs TAVILY_API_KEY                         (AI-search)
    ///   exa       — needs EXA_SECRET_KEY                         (neural; site: auto-converted)
    ///   firecrawl — needs FIRECRAWL_DEV_API_KEY                  (Google via firecrawl.dev)
    Discover {
        /// ATS name to discover for (greenhouse | ashby | lever |
        /// smartrecruiters | bamboohr | recruitee | workday), or "all"
        name: String,
        /// Search engine backend to use.
        #[arg(long, default_value = "brave")]
        engine: String,
    },
    /// Import apply URLs you collected by hand. Each URL is classified via
    /// the same `ats::classify_apply_url` used by crawlers — recognized
    /// hits upsert a `companies` row that the next `sync` will pull jobs
    /// for. Unrecognized URLs are skipped with a warning.
    ///
    /// Pass URLs as positional arguments, or omit them and pipe one URL per
    /// line on stdin (lines starting with `#` and blank lines are ignored).
    ///
    ///   ajs import https://boards.greenhouse.io/parity/jobs/4567
    ///   ajs import < urls.txt
    ///   pbpaste | ajs import
    Import {
        /// URLs to import. If empty, reads stdin.
        urls: Vec<String>,
    },
    /// List rows from the database
    List {
        #[command(subcommand)]
        what: ListWhat,
    },
    /// Print the database location and quick stats
    Status,
    /// Tag a job with a personal status: `applied`, `dismissed`, or `reset`
    /// (clears back to `new`). The id is the integer in the first column of
    /// `list jobs` output — the SQLite primary key, not the apply URL.
    Mark {
        /// SQLite `jobs.id` — first column of `list jobs` output. NOT the URL.
        id: i64,
        /// `applied` | `dismissed` | `reset`
        status: String,
        /// Optional note (e.g. why dismissed, who referred, link to thread)
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ListWhat {
    Companies {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Jobs {
        /// Max rows to return. Unlimited if omitted.
        #[arg(long)]
        limit: Option<usize>,
        /// Skip this many rows before returning (paginates with --limit).
        /// `--start 1000 --limit 50` → rows 1000..1049.
        #[arg(long, default_value_t = 0)]
        start: usize,
        /// Only show jobs the ATS flagged as remote.
        #[arg(long)]
        remote: bool,
        /// FTS5 query over title/location/department/description.
        /// Quote terms with punctuation, e.g. `"c++"`.
        #[arg(long, value_name = "QUERY")]
        r#match: Option<String>,
        /// Include rows you previously marked `dismissed` (hidden by default).
        #[arg(long, conflicts_with = "applied")]
        all: bool,
        /// Show only rows you marked `applied`.
        #[arg(long, conflicts_with = "all")]
        applied: bool,
    },
    /// Print every company that has open jobs, with its jobs indented underneath.
    ByCompany {
        #[arg(long, default_value_t = 1000)]
        limit: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,reqwest=warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let db = Db::open(&cli.db)?;

    match cli.cmd {
        Cmd::Crawl { name } => {
            let to_run: Vec<Box<dyn crawlers::Crawler>> = if name == "all" {
                crawlers::all()
            } else {
                let known: Vec<&'static str> = crawlers::all().iter().map(|c| c.name()).collect();
                let c = crawlers::by_name(&name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown crawler '{name}'. known: {} (or 'all')",
                        known.join(", ")
                    )
                })?;
                vec![c]
            };
            let mut failed = 0usize;
            for crawler in to_run {
                let run_id = db.start_run(crawler.name())?;
                match crawler.run(&db).await {
                    Ok(rep) => {
                        db.finish_run(
                            run_id,
                            true,
                            rep.http_status,
                            rep.jobs_matched,
                            rep.jobs_new,
                            None,
                        )?;
                        info!(
                            source = rep.source,
                            pages = rep.pages_visited,
                            apply_links = rep.apply_links_found,
                            jobs_matched = rep.jobs_matched,
                            jobs_new = rep.jobs_new,
                            companies_new = rep.companies_new,
                            "crawl finished"
                        );
                    }
                    Err(e) => {
                        db.finish_run(run_id, false, None, 0, 0, Some(&e.to_string()))?;
                        error!(source = crawler.name(), error = %e, "crawl failed");
                        failed += 1;
                    }
                }
            }
            if failed > 0 {
                anyhow::bail!("{failed} crawler(s) failed");
            }
        }
        Cmd::Sync { name } => {
            let to_run: Vec<Box<dyn adapters::AtsAdapter>> = if name == "all" {
                adapters::all()
            } else {
                let known: Vec<&'static str> =
                    adapters::all().iter().map(|a| a.kind().as_str()).collect();
                let a = adapters::by_name(&name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown ATS '{name}'. known: {} (or 'all')",
                        known.join(", ")
                    )
                })?;
                vec![a]
            };
            for adapter in to_run {
                let label = format!("sync:{}", adapter.kind().as_str());
                let run_id = db.start_run(&label)?;
                match adapters::sync_all_for_kind(&db, adapter.as_ref()).await {
                    Ok(rep) => {
                        db.finish_run(run_id, true, None, rep.jobs_seen, rep.jobs_new, None)?;
                        info!(
                            kind = rep.kind,
                            companies = rep.companies_synced,
                            stale_404 = rep.companies_404,
                            jobs_seen = rep.jobs_seen,
                            jobs_new = rep.jobs_new,
                            jobs_closed = rep.jobs_closed,
                            "sync finished"
                        );
                    }
                    Err(e) => {
                        db.finish_run(run_id, false, None, 0, 0, Some(&e.to_string()))?;
                        error!(kind = adapter.kind().as_str(), error = %e, "sync failed");
                        return Err(e);
                    }
                }
            }
        }
        Cmd::List { what } => match what {
            ListWhat::Companies { limit } => {
                for (name, kind, slug) in db.list_companies(limit)? {
                    println!("{kind:<16} {slug:<32} {name}");
                }
            }
            ListWhat::Jobs {
                limit,
                start,
                remote,
                r#match,
                all,
                applied,
            } => {
                let status_filter = if applied {
                    StatusFilter::AppliedOnly
                } else if all {
                    StatusFilter::All
                } else {
                    StatusFilter::HideDismissed
                };
                let rows =
                    db.list_jobs_filtered(limit, start, remote, r#match.as_deref(), status_filter)?;
                for row in rows {
                    let remote_tag = if row.remote == Some(true) {
                        " [remote]"
                    } else {
                        ""
                    };
                    let status_tag = match row.status.as_str() {
                        "applied" => " [applied]",
                        "dismissed" => " [dismissed]",
                        _ => "",
                    };
                    println!(
                        "{id:>6}  {company} | {title}{remote_tag}{status_tag} | {location} | {url}",
                        id = row.id,
                        company = row.company,
                        title = row.title,
                        location = row.location,
                        url = row.apply_url,
                    );
                }
            }
            ListWhat::ByCompany { limit } => {
                for cwj in db.list_by_company(limit)? {
                    println!("{} ({}:{})", cwj.name, cwj.kind, cwj.slug);
                    for job in cwj.jobs {
                        if job.location.is_empty() {
                            println!("  - {}", job.title);
                        } else {
                            println!("  - {} | {}", job.title, job.location);
                        }
                        println!("    {}", job.apply_url);
                    }
                    println!();
                }
            }
        },
        Cmd::Status => {
            println!("db: {}", cli.db.display());
            let companies = db.list_companies(usize::MAX)?.len();
            let jobs = db.list_jobs(usize::MAX)?.len();
            println!("companies: {companies}");
            println!("open jobs: {jobs}");
        }
        Cmd::Discover { name, engine } => {
            let engine = search::from_env(&engine)?;
            let plans: Vec<&'static discover::DiscoverPlan> = if name == "all" {
                discover::PLANS.iter().collect()
            } else {
                let p = discover::plan_for(&name).ok_or_else(|| {
                    let known: Vec<&str> =
                        discover::PLANS.iter().map(|p| p.kind.as_str()).collect();
                    anyhow::anyhow!(
                        "unknown ATS '{name}'. known: {} (or 'all')",
                        known.join(", ")
                    )
                })?;
                vec![p]
            };
            for plan in plans {
                let label = format!("discover:{}", plan.kind.as_str());
                let run_id = db.start_run(&label)?;
                match discover::run_plan(&db, engine.as_ref(), plan).await {
                    Ok(rep) => {
                        db.finish_run(run_id, true, None, rep.urls_seen, rep.companies_new, None)?;
                        info!(
                            kind = rep.kind,
                            queries = rep.queries_sent,
                            urls = rep.urls_seen,
                            classified = rep.urls_classified,
                            companies_new = rep.companies_new,
                            companies_seen = rep.companies_seen,
                            "discover finished"
                        );
                    }
                    Err(e) => {
                        db.finish_run(run_id, false, None, 0, 0, Some(&e.to_string()))?;
                        error!(kind = plan.kind.as_str(), error = %e, "discover failed");
                        return Err(e);
                    }
                }
            }
        }
        Cmd::Import { urls } => {
            let urls: Vec<String> = if urls.is_empty() {
                use std::io::BufRead;
                std::io::stdin()
                    .lock()
                    .lines()
                    .map_while(Result::ok)
                    .collect()
            } else {
                urls
            };
            let run_id = db.start_run("import:manual")?;
            let mut imported_new = 0u64;
            let mut already_known = 0u64;
            let mut skipped = 0u64;
            for raw in &urls {
                let url = raw.trim();
                if url.is_empty() || url.starts_with('#') {
                    continue;
                }
                let Some(refr) = ats::classify_apply_url(url) else {
                    tracing::warn!(url, "unrecognized — not a known ATS, skipping");
                    skipped += 1;
                    continue;
                };
                match db.upsert_company(None, refr.kind, &refr.slug, "import:manual", Some(url)) {
                    Ok((_, is_new)) => {
                        if is_new {
                            imported_new += 1;
                            info!(kind = refr.kind.as_str(), slug = %refr.slug, url, "imported");
                        } else {
                            already_known += 1;
                            info!(kind = refr.kind.as_str(), slug = %refr.slug, "already known");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, url, "company upsert failed");
                        skipped += 1;
                    }
                }
            }
            db.finish_run(run_id, true, None, imported_new + already_known, imported_new, None)?;
            println!(
                "imported {imported_new} new, {already_known} already known, {skipped} skipped"
            );
            if imported_new > 0 {
                println!(
                    "Run `ajs sync all` (or `ajs sync <ats>`) to pull jobs for the new companies."
                );
            }
        }
        Cmd::Mark { id, status, note } => {
            let canonical = match status.as_str() {
                "reset" | "new" => "new",
                "applied" => "applied",
                "dismissed" => "dismissed",
                other => anyhow::bail!(
                    "unknown status '{other}' (expected: applied | dismissed | reset)"
                ),
            };
            db.set_status(id, canonical, note.as_deref())?;
            println!("job {id}: {canonical}");
        }
    }
    Ok(())
}
