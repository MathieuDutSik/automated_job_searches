use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Greenhouse;

#[derive(Deserialize, Debug)]
struct GhResp {
    jobs: Vec<GhJob>,
}

#[derive(Deserialize, Debug)]
struct GhJob {
    id: serde_json::Value,
    title: String,
    absolute_url: String,
    #[serde(default)]
    location: Option<GhLocation>,
    #[serde(default)]
    departments: Vec<GhDept>,
    updated_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GhLocation {
    name: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GhDept {
    name: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for Greenhouse {
    fn kind(&self) -> AtsKind {
        AtsKind::Greenhouse
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=false");
        let client = http::client()?;
        let Some(resp): Option<GhResp> = fetch_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        Ok(resp
            .jobs
            .into_iter()
            .map(|j| AdapterJob {
                external_id: j.id.to_string().trim_matches('"').to_string(),
                title: j.title,
                location: j.location.and_then(|l| l.name),
                department: j.departments.first().and_then(|d| d.name.clone()),
                apply_url: j.absolute_url,
                posted_at: j.updated_at,
                raw_json: "{}".to_string(),
            })
            .collect())
    }
}
