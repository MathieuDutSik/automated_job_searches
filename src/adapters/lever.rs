use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_value_or_none_on_404, AdapterJob, AtsAdapter};
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
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
    #[serde(default, rename = "workplaceType")]
    workplace_type: Option<String>,
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
        let Some(value) = fetch_value_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        let jobs_arr = value.as_array().cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(jobs_arr.len());
        for entry in jobs_arr {
            let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
            let j: LeverJob = match serde_json::from_value(entry) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let (location, team) = match j.categories {
                Some(c) => (c.location, c.team),
                None => (None, None),
            };
            let apply_url = j
                .hosted_url
                .unwrap_or_else(|| format!("https://jobs.lever.co/{slug}/{}", j.id));
            let posted_at = j
                .created_at
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|d| d.to_rfc3339());
            let remote = j
                .workplace_type
                .as_deref()
                .map(|w| w.eq_ignore_ascii_case("remote"));
            out.push(AdapterJob {
                external_id: j.id,
                title: j.text,
                location,
                department: team,
                apply_url,
                description: j.description_plain,
                remote,
                posted_at,
                raw_json,
            });
        }
        Ok(out)
    }
}
