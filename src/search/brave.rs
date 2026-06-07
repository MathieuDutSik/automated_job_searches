use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const NAME: &str = "brave";

pub struct Brave {
    api_key: String,
}

impl Brave {
    /// Construct from a key already in hand. Use `from_env()` to read
    /// `BRAVE_API_KEY` and produce a clear error if it's unset.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("BRAVE_API_KEY").context("BRAVE_API_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("BRAVE_API_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Deserialize, Debug)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize, Debug)]
struct BraveResult {
    url: String,
}

#[async_trait(?Send)]
impl SearchEngine for Brave {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        // Brave caps `count` at 20 per request.
        let count = count.min(20);
        let resp = client
            .get(ENDPOINT)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query), ("count", &count.to_string())])
            .send()
            .await
            .with_context(|| format!("Brave search GET {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Brave search returned {}: {}", status, body);
        }
        let parsed: BraveResponse = resp.json().await.context("parse Brave JSON")?;
        let hits = parsed
            .web
            .map(|w| w.results)
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchHit { url: r.url })
            .collect();
        Ok(hits)
    }
}
