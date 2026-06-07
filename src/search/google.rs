use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://customsearch.googleapis.com/customsearch/v1";
const NAME: &str = "google";

pub struct Google {
    api_key: String,
    cse_id: String,
}

impl Google {
    pub fn new(api_key: String, cse_id: String) -> Self {
        Self { api_key, cse_id }
    }

    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("GOOGLE_API_KEY").context("GOOGLE_API_KEY env var not set")?;
        let cse_id = std::env::var("GOOGLE_CSE_ID")
            .context("GOOGLE_CSE_ID env var not set (create a Programmable Search Engine at https://programmablesearchengine.google.com/ with \"Search the entire web\" enabled, then export its ID)")?;
        if api_key.trim().is_empty() || cse_id.trim().is_empty() {
            anyhow::bail!("GOOGLE_API_KEY or GOOGLE_CSE_ID is empty");
        }
        Ok(Self::new(api_key, cse_id))
    }
}

#[derive(Deserialize, Debug)]
struct GoogleResponse {
    #[serde(default)]
    items: Vec<GoogleItem>,
}

#[derive(Deserialize, Debug)]
struct GoogleItem {
    link: String,
}

#[async_trait(?Send)]
impl SearchEngine for Google {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        // Google CSE caps `num` at 10 per request. For deeper paging we'd
        // need to add a `start` param and loop; left for later.
        let count = count.min(10);
        let resp = client
            .get(ENDPOINT)
            .query(&[
                ("key", self.api_key.as_str()),
                ("cx", self.cse_id.as_str()),
                ("q", query),
                ("num", &count.to_string()),
            ])
            .send()
            .await
            .with_context(|| format!("Google CSE GET {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Google CSE returned {}: {}", status, body);
        }
        let parsed: GoogleResponse = resp.json().await.context("parse Google CSE JSON")?;
        Ok(parsed
            .items
            .into_iter()
            .map(|i| SearchHit { url: i.link })
            .collect())
    }
}
