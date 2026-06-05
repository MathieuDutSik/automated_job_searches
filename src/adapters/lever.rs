use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Lever;

#[derive(Deserialize, Debug)]
struct LeverJob {
    id: String,
    text: String,
    #[serde(default)]
    categories: Option<LeverCategories>,
    #[serde(default, rename = "hostedUrl")]
    hosted_url: Option<String>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<i64>,
}

#[derive(Deserialize, Debug)]
struct LeverCategories {
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    team: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for Lever {
    fn kind(&self) -> AtsKind {
        AtsKind::Lever
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://api.lever.co/v0/postings/{slug}?mode=json");
        let client = http::client()?;
        let Some(jobs): Option<Vec<LeverJob>> = fetch_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        Ok(jobs
            .into_iter()
            .map(|j| {
                let (location, team) = match j.categories {
                    Some(c) => (c.location, c.team),
                    None => (None, None),
                };
                let apply_url = j
                    .hosted_url
                    .unwrap_or_else(|| format!("https://jobs.lever.co/{slug}/{}", j.id));
                let posted_at = j
                    .created_at
                    .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
                    .map(|d| d.to_rfc3339());
                AdapterJob {
                    external_id: j.id,
                    title: j.text,
                    location,
                    department: team,
                    apply_url,
                    posted_at,
                    raw_json: "{}".to_string(),
                }
            })
            .collect())
    }
}
