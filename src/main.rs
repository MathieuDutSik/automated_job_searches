use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod adapters;
mod ats;
mod crawlers;
mod db;
mod http;

use crate::db::Db;

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
        /// ATS name (greenhouse | ashby | lever), or "all"
        name: String,
    },
    /// List rows from the database
    List {
        #[command(subcommand)]
        what: ListWhat,
    },
    /// Print the database location and quick stats
    Status,
}

#[derive(Subcommand, Debug)]
enum ListWhat {
    Companies {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Jobs {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Only show jobs the ATS flagged as remote.
        #[arg(long)]
        remote: bool,
        /// FTS5 query over title/location/department/description.
        /// Trigram tokenizer — quote terms with punctuation, e.g. `"c++"`.
        #[arg(long, value_name = "QUERY")]
        r#match: Option<String>,
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
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,reqwest=warn")))
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
                    anyhow::anyhow!("unknown crawler '{name}'. known: {} (or 'all')", known.join(", "))
                })?;
                vec![c]
            };
            let mut failed = 0usize;
            for crawler in to_run {
                let run_id = db.start_run(crawler.name())?;
                match crawler.run(&db).await {
                    Ok(rep) => {
                        db.finish_run(run_id, true, rep.http_status, rep.jobs_matched, rep.jobs_new, None)?;
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
            ListWhat::Jobs { limit, remote, r#match } => {
                let rows = db.list_jobs_filtered(limit, remote, r#match.as_deref())?;
                for (company, title, location, url, remote_flag) in rows {
                    let tag = match remote_flag {
                        Some(true) => " [remote]",
                        _ => "",
                    };
                    println!("{company} | {title}{tag} | {location} | {url}");
                }
            }
            ListWhat::ByCompany { limit } => {
                for (name, kind, slug, jobs) in db.list_by_company(limit)? {
                    println!("{name} ({kind}:{slug})");
                    for (title, location, url) in jobs {
                        if location.is_empty() {
                            println!("  - {title}");
                        } else {
                            println!("  - {title} | {location}");
                        }
                        println!("    {url}");
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
    }
    Ok(())
}
