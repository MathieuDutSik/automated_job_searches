use anyhow::Result;
use async_trait::async_trait;

use crate::db::Db;

pub mod cryptocurrencyjobs;
pub mod hn_whoshiring;
pub mod web3career;

#[derive(Debug, Default)]
pub struct CrawlReport {
    pub source: &'static str,
    pub http_status: Option<u16>,
    /// Number of job-detail pages the crawler visited.
    pub pages_visited: u64,
    /// Detail pages from which an outbound apply URL was extracted.
    pub apply_links_found: u64,
    /// Apply URLs classified into an AtsKind (including `Other`).
    pub jobs_matched: u64,
    /// New rows inserted into `jobs`.
    pub jobs_new: u64,
    /// New rows inserted into `companies`.
    pub companies_new: u64,
}

#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}

pub fn all() -> Vec<Box<dyn Crawler>> {
    vec![
        Box::new(cryptocurrencyjobs::CryptocurrencyJobs),
        Box::new(hn_whoshiring::HnWhosHiring),
        Box::new(web3career::Web3Career),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Crawler>> {
    all().into_iter().find(|c| c.name() == name)
}
