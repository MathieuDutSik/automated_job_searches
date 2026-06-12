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
        /// ATS name (greenhouse | ashbyhq | lever | smartrecruiters | bamboohr |
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
        /// ATS name to discover for (greenhouse | ashbyhq | lever |
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
    /// Tag a job (or several) with a personal status: `applied`,
    /// `dismissed`, or `reset` (clears back to `new`). Ids are the integers
    /// in the first column of `list jobs` output — SQLite primary keys, not
    /// apply URLs.
    ///
    /// The selector can be one id, a comma-separated list of ids, or a
    /// company specifier prefixed with `company:` to mark every open job
    /// of that company in one shot. The note (if any) applies to all
    /// affected rows.
    ///
    ///   ajs mark 13105 dismissed
    ///   ajs mark 13105,13090,13091 dismissed --note "stack mismatch"
    ///   ajs mark company:parity dismissed
    ///   ajs mark company:greenhouse/parity dismissed   # disambiguate kind
    Mark {
        /// One id, comma-separated id list, or `company:[kind/]slug`.
        selector: String,
        /// `applied` | `dismissed` | `reset`
        status: String,
        /// Optional note (e.g. why dismissed, who referred, link to thread).
        /// Applied to every affected row.
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
                let now = chrono::Utc::now();
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
                    let age = row
                        .posted_at
                        .as_deref()
                        .map(|s| format_age(s, now))
                        .unwrap_or_else(|| "      ?".to_string());
                    println!(
                        "{id:>6}  {age:>7}  {company} | {title}{remote_tag}{status_tag} | {location} | {url}",
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
            // (kind, company_id, slug) for each newly-inserted company — used
            // below to auto-sync only those, instead of every company of that
            // kind in the DB.
            let mut fresh: Vec<(ats::AtsKind, i64, String)> = Vec::new();
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
                    Ok((company_id, is_new)) => {
                        if is_new {
                            imported_new += 1;
                            fresh.push((refr.kind, company_id, refr.slug.clone()));
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

            // Auto-sync the newly imported companies — only those, not every
            // company sharing the same ATS kind. Group by kind so we build
            // each adapter at most once.
            if !fresh.is_empty() {
                use std::collections::BTreeMap;
                let mut by_kind: BTreeMap<&'static str, Vec<(i64, String)>> = BTreeMap::new();
                for (kind, id, slug) in fresh {
                    by_kind.entry(kind.as_str()).or_default().push((id, slug));
                }
                let mut total_jobs_new = 0u64;
                let mut total_synced = 0u64;
                let mut skipped_no_adapter: Vec<&'static str> = Vec::new();
                for (kind_name, slugs) in by_kind {
                    let Some(adapter) = adapters::by_name(kind_name) else {
                        skipped_no_adapter.push(kind_name);
                        continue;
                    };
                    for (idx, (company_id, slug)) in slugs.into_iter().enumerate() {
                        if idx > 0 {
                            tokio::time::sleep(adapters::POLITENESS_DELAY).await;
                        }
                        match adapters::sync_one_slug(
                            &db,
                            adapter.as_ref(),
                            company_id,
                            &slug,
                            &slug,
                        )
                        .await
                        {
                            Ok(r) => {
                                total_synced += r.companies_synced;
                                total_jobs_new += r.jobs_new;
                            }
                            Err(e) => tracing::warn!(error = %e, slug, "auto-sync failed"),
                        }
                    }
                }
                println!(
                    "auto-synced {total_synced} new compan{plural}, {total_jobs_new} jobs pulled",
                    plural = if total_synced == 1 { "y" } else { "ies" }
                );
                for kind in skipped_no_adapter {
                    println!(
                        "  note: no adapter for `{kind}` yet — companies stored but not synced"
                    );
                }
            }
        }
        Cmd::Mark { selector, status, note } => {
            let canonical = match status.as_str() {
                "reset" | "new" => "new",
                "applied" => "applied",
                "dismissed" => "dismissed",
                other => anyhow::bail!(
                    "unknown status '{other}' (expected: applied | dismissed | reset)"
                ),
            };
            if let Some(spec) = selector.strip_prefix("company:") {
                let (kind_filter, slug) = match spec.split_once('/') {
                    Some((k, s)) => (Some(k), s),
                    None => (None, spec),
                };
                if slug.is_empty() {
                    anyhow::bail!("empty slug in `company:` selector");
                }
                let matches = db.find_companies_by_slug(slug, kind_filter)?;
                let c = match matches.as_slice() {
                    [] => anyhow::bail!(
                        "no company matches `{selector}`. Check the slug with `ajs list companies`."
                    ),
                    [only] => only,
                    many => {
                        let kinds: Vec<String> =
                            many.iter().map(|m| format!("{}:{}", m.kind, m.slug)).collect();
                        anyhow::bail!(
                            "slug `{slug}` is ambiguous across {} kinds — re-run with `company:kind/slug`. Matches: {}",
                            many.len(),
                            kinds.join(", ")
                        );
                    }
                };
                let n = db.set_status_for_company(c.id, canonical, note.as_deref())?;
                println!(
                    "marked {n} open job{plural} of {name} ({kind}:{slug}) as {canonical}",
                    plural = if n == 1 { "" } else { "s" },
                    name = c.name,
                    kind = c.kind,
                    slug = c.slug,
                );
            } else {
                let parsed: Vec<i64> = selector
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        s.parse::<i64>()
                            .map_err(|_| anyhow::anyhow!("not a valid id: '{s}'"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                if parsed.is_empty() {
                    anyhow::bail!("no ids given");
                }
                let mut ok = 0usize;
                let mut failed = 0usize;
                for id in &parsed {
                    match db.set_status(*id, canonical, note.as_deref()) {
                        Ok(()) => {
                            ok += 1;
                            println!("job {id}: {canonical}");
                        }
                        Err(e) => {
                            failed += 1;
                            eprintln!("job {id}: failed — {e}");
                        }
                    }
                }
                if parsed.len() > 1 {
                    println!("marked {ok}/{} ({failed} failed)", parsed.len());
                }
            }
        }
    }
    Ok(())
}

/// Render a `posted_at` value as an age in days (e.g. `12d`). Tries RFC3339
/// first (Ashby/Greenhouse/Lever/SmartRecruiters), then the
/// `YYYY-MM-DD HH:MM:SS UTC` shape Recruitee returns. Workday's free-form
/// `postedOn` (e.g. "Posted 10 Days Ago", "Posted Today", "Posted 30+ Days
/// Ago") is parsed by phrase. Anything unrecognized falls back to the raw
/// string with a leading `Posted ` stripped.
fn format_age(posted_at: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let when = chrono::DateTime::parse_from_rfc3339(posted_at)
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(posted_at, "%Y-%m-%d %H:%M:%S UTC")
                .map(|d| d.and_utc())
        });
    if let Ok(when) = when {
        let days = now.signed_duration_since(when).num_days().max(0);
        return format!("{days}d");
    }
    let phrase = posted_at.trim_start_matches("Posted ").trim();
    if phrase.eq_ignore_ascii_case("today") {
        return "0d".to_string();
    }
    if phrase.eq_ignore_ascii_case("yesterday") {
        return "1d".to_string();
    }
    if let Some(rest) = phrase.strip_suffix(" Days Ago").or_else(|| phrase.strip_suffix(" Day Ago")) {
        // "30+" → ">30d", "10" → "10d"
        if let Some(n) = rest.strip_suffix('+') {
            return format!(">{n}d");
        }
        if rest.parse::<u32>().is_ok() {
            return format!("{rest}d");
        }
    }
    phrase.to_string()
}
