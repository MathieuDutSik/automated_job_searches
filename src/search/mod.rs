use anyhow::Result;
use async_trait::async_trait;

pub mod brave;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub url: String,
}

#[async_trait(?Send)]
pub trait SearchEngine {
    /// Human-readable name, used for `discovered_via` / log tagging.
    fn name(&self) -> &'static str;

    /// Run a single web-search query and return up to `count` results.
    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>>;
}
