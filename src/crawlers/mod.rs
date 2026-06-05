use anyhow::Result;
use async_trait::async_trait;

use crate::db::Db;

pub mod web3career;

#[derive(Debug, Default)]
pub struct CrawlReport {
    pub source: &'static str,
    pub http_status: Option<u16>,
    pub companies_seen: u64,
    pub companies_new: u64,
}

#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}

pub fn by_name(name: &str) -> Option<Box<dyn Crawler>> {
    match name {
        "web3career" => Some(Box::new(web3career::Web3Career)),
        _ => None,
    }
}
