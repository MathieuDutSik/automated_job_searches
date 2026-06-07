use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://api.ydc-index.io/search";
const NAME: &str = "you";

pub struct You {
    api_key: String,
}

impl You {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("YDC_API_KEY").context("YDC_API_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("YDC_API_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct YouResponse {
    #[serde(default)]
    hits: Vec<YouHit>,
}

#[derive(Deserialize, Debug)]
struct YouHit {
    url: String,
}

#[async_trait(?Send)]
impl SearchEngine for You {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        let resp = client
            .get(ENDPOINT)
            .header("X-API-Key", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("query", query), ("num_web_results", &count.to_string())])
            .send()
            .await
            .with_context(|| format!("You.com GET {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("You.com returned {}: {}", status, body);
        }
        let parsed: YouResponse = resp.json().await.context("parse You.com JSON")?;
        Ok(parsed
            .hits
            .into_iter()
            .map(|h| SearchHit { url: h.url })
            .collect())
    }
}
