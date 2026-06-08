use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://google.serper.dev/search";
const NAME: &str = "serper";

/// Serper.dev — Google search wrapped behind a clean JSON API. Free tier
/// is ~2500 credits, paid is one of the cheaper SerpAPI alternatives. Our
/// queries cost 1 credit each.
pub struct Serper {
    api_key: String,
}

impl Serper {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("SERPER_API_KEY").context("SERPER_API_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("SERPER_API_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct SerperResponse {
    #[serde(default)]
    organic: Vec<SerperHit>,
}

#[derive(Deserialize, Debug)]
struct SerperHit {
    link: String,
}

#[async_trait(?Send)]
impl SearchEngine for Serper {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        let body = json!({ "q": query, "num": count });
        let resp = client
            .post(ENDPOINT)
            .header("X-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("Serper POST {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Serper returned {}: {}", status, body);
        }
        let parsed: SerperResponse = resp.json().await.context("parse Serper JSON")?;
        Ok(parsed
            .organic
            .into_iter()
            .map(|h| SearchHit { url: h.link })
            .collect())
    }
}
