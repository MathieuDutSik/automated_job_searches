use anyhow::Result;
use async_trait::async_trait;

pub mod brave;
pub mod exa;
pub mod firecrawl;
pub mod google;
pub mod serper;
pub mod tavily;

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

/// Names of every backend the CLI accepts for `discover --engine`.
pub const ENGINE_NAMES: &[&str] = &[
    "brave",
    "exa",
    "firecrawl",
    "google",
    "serper",
    "tavily",
];

/// Construct an engine by short name, reading its env vars. Returns a
/// boxed trait object so the caller (CLI) doesn't need to know which
/// concrete type came back.
pub fn from_env(name: &str) -> Result<Box<dyn SearchEngine>> {
    match name {
        "brave" => Ok(Box::new(brave::Brave::from_env()?)),
        "exa" => Ok(Box::new(exa::Exa::from_env()?)),
        "firecrawl" => Ok(Box::new(firecrawl::Firecrawl::from_env()?)),
        "google" => Ok(Box::new(google::Google::from_env()?)),
        "serper" => Ok(Box::new(serper::Serper::from_env()?)),
        "tavily" => Ok(Box::new(tavily::Tavily::from_env()?)),
        other => anyhow::bail!(
            "unknown search engine '{other}'. known: {}",
            ENGINE_NAMES.join(", ")
        ),
    }
}
