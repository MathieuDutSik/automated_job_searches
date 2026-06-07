use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_value_or_none_on_404, html_to_text, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Greenhouse;

#[derive(Deserialize, Debug)]
struct GhJob {
    id: serde_json::Value,
    title: String,
    absolute_url: String,
    #[serde(default)]
    location: Option<GhLocation>,
    #[serde(default)]
    offices: Vec<GhOffice>,
    #[serde(default)]
    departments: Vec<GhDept>,
    #[serde(default)]
    content: Option<String>,
    updated_at: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GhLocation {
    name: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GhOffice {
    name: Option<String>,
    #[serde(default)]
    location: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GhDept {
    name: Option<String>,
}

fn looks_remote(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("remote") || l.contains("anywhere") || l.contains("work from home")
}

#[async_trait(?Send)]
impl AtsAdapter for Greenhouse {
    fn kind(&self) -> AtsKind {
        AtsKind::Greenhouse
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true");
        let client = http::client()?;
        let Some(value) = fetch_value_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        let jobs_arr = value
            .get("jobs")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(jobs_arr.len());
        for entry in jobs_arr {
            let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
            let j: GhJob = match serde_json::from_value(entry) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let location_name = j.location.and_then(|l| l.name);
            let mut remote_flag = location_name.as_deref().map(looks_remote).unwrap_or(false);
            for o in &j.offices {
                if let Some(n) = &o.name {
                    if looks_remote(n) {
                        remote_flag = true;
                    }
                }
                if let Some(l) = &o.location {
                    if looks_remote(l) {
                        remote_flag = true;
                    }
                }
            }
            let description = j.content.as_deref().map(html_to_text);
            out.push(AdapterJob {
                external_id: j.id.to_string().trim_matches('"').to_string(),
                title: j.title,
                location: location_name,
                department: j.departments.first().and_then(|d| d.name.clone()),
                apply_url: j.absolute_url,
                description,
                remote: Some(remote_flag),
                posted_at: j.updated_at,
                raw_json,
            });
        }
        Ok(out)
    }
}
