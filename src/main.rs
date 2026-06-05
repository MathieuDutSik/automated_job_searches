use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

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
                        db.finish_run(run_id, true, rep.http_status, rep.companies_matched, rep.companies_new, None)?;
                        info!(
                            source = rep.source,
                            links_examined = rep.links_examined,
                            companies_matched = rep.companies_matched,
                            companies_new = rep.companies_new,
                            "crawl finished"
                        );
                        if rep.links_examined > 0 && rep.companies_matched == 0 {
                            tracing::warn!(
                                source = rep.source,
                                "0 ATS matches: apply links appear to go through the site's own redirect (e.g. /apply/<id>) rather than directly to boards.greenhouse.io / jobs.ashbyhq.com / etc. Run with RUST_LOG=ajs=debug to dump the URLs the crawler saw."
                            );
                        }
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
        Cmd::List { what } => match what {
            ListWhat::Companies { limit } => {
                for (name, kind, slug) in db.list_companies(limit)? {
                    println!("{kind:<16} {slug:<32} {name}");
                }
            }
            ListWhat::Jobs { limit } => {
                for (company, title, location, url) in db.list_jobs(limit)? {
                    println!("{company} | {title} | {location} | {url}");
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
