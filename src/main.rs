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
    /// Path to the SQLite database
    #[arg(long, global = true, default_value = "jobs.db")]
    db: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a crawler against an aggregator site
    Crawl {
        /// Crawler name (e.g. web3career)
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
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,reqwest=warn,html5ever=error")))
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let db = Db::open(&cli.db)?;

    match cli.cmd {
        Cmd::Crawl { name } => {
            let crawler = crawlers::by_name(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown crawler '{name}'. known: web3career"))?;
            let run_id = db.start_run(crawler.name())?;
            match crawler.run(&db).await {
                Ok(rep) => {
                    db.finish_run(run_id, true, rep.http_status, rep.companies_seen, rep.companies_new, None)?;
                    info!(
                        source = rep.source,
                        seen = rep.companies_seen,
                        new = rep.companies_new,
                        "crawl finished"
                    );
                }
                Err(e) => {
                    db.finish_run(run_id, false, None, 0, 0, Some(&e.to_string()))?;
                    error!(error = %e, "crawl failed");
                    return Err(e);
                }
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
