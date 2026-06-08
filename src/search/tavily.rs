use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://api.tavily.com/search";
const NAME: &str = "tavily";

/// Tavily — AI-search API targeted at LLM/agent use cases. Free dev tier
/// is generous (~1000 credits/mo). `site:` queries work but recall is
/// uneven; useful as a cross-check against Serper/Google.
pub struct Tavily {
    api_key: String,
}

impl Tavily {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("TAVILY_API_KEY").context("TAVILY_API_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("TAVILY_API_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyHit>,
}

#[derive(Deserialize, Debug)]
struct TavilyHit {
    url: String,
}

#[async_trait(?Send)]
impl SearchEngine for Tavily {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        let body = json!({
            "query": query,
            "max_results": count,
            "search_depth": "basic",
            "include_answer": false,
            "include_images": false,
        });
        let resp = client
            .post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("Tavily POST {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tavily returned {}: {}", status, body);
        }
        let parsed: TavilyResponse = resp.json().await.context("parse Tavily JSON")?;
        Ok(parsed
            .results
            .into_iter()
            .map(|h| SearchHit { url: h.url })
            .collect())
    }
}
