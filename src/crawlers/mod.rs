use anyhow::Result;
use async_trait::async_trait;

use crate::db::Db;

pub mod web3career;

#[derive(Debug, Default)]
pub struct CrawlReport {
    pub source: &'static str,
    pub http_status: Option<u16>,
    /// Outbound links the crawler examined (e.g. <a href>s on the page).
    pub links_examined: u64,
    /// Links that matched a known ATS URL pattern.
    pub companies_matched: u64,
    /// New rows inserted into `companies` (excludes existing rows that were just refreshed).
    pub companies_new: u64,
}

#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}

pub fn all() -> Vec<Box<dyn Crawler>> {
    vec![Box::new(web3career::Web3Career)]
}

pub fn by_name(name: &str) -> Option<Box<dyn Crawler>> {
    all().into_iter().find(|c| c.name() == name)
}
