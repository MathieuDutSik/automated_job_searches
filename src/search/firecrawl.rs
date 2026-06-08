use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://api.firecrawl.dev/v1/search";
const NAME: &str = "firecrawl";

/// Firecrawl — primarily a scraper, but they expose a search endpoint that
/// returns Google-quality results (Google is the backing index under the
/// hood). Free tier is 500 credits. Returns the same URL set as Serper for
/// the same query, so this is mostly useful as a fallback / cross-check.
pub struct Firecrawl {
    api_key: String,
}

impl Firecrawl {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("FIRECRAWL_DEV_API_KEY")
            .context("FIRECRAWL_DEV_API_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("FIRECRAWL_DEV_API_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct FirecrawlResponse {
    #[serde(default)]
    data: Vec<FirecrawlHit>,
}

#[derive(Deserialize, Debug)]
struct FirecrawlHit {
    url: String,
}

#[async_trait(?Send)]
impl SearchEngine for Firecrawl {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        let body = json!({ "query": query, "limit": count });
        let resp = client
            .post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("Firecrawl POST {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Firecrawl returned {}: {}", status, body);
        }
        let parsed: FirecrawlResponse =
            resp.json().await.context("parse Firecrawl JSON")?;
        Ok(parsed
            .data
            .into_iter()
            .map(|h| SearchHit { url: h.url })
            .collect())
    }
}
