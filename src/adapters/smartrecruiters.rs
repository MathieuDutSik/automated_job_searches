use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

use crate::adapters::{AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct SmartRecruiters;

const PAGE_LIMIT: u32 = 100;
const PAGE_DELAY: Duration = Duration::from_millis(150);

#[derive(Deserialize, Debug)]
struct SrPage {
    #[serde(default, rename = "totalFound")]
    total_found: u32,
    #[serde(default)]
    content: Vec<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct SrLocation {
    #[serde(default, rename = "fullLocation")]
    full_location: Option<String>,
    #[serde(default)]
    remote: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct SrLabeled {
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SrPosting {
    id: String,
    name: String,
    #[serde(default)]
    location: Option<SrLocation>,
    #[serde(default)]
    department: Option<SrLabeled>,
    #[serde(default, rename = "releasedDate")]
    released_date: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for SmartRecruiters {
    fn kind(&self) -> AtsKind {
        AtsKind::Smartrecruiters
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let client = http::client()?;
        let mut out: Vec<AdapterJob> = Vec::new();
        let mut offset: u32 = 0;
        loop {
            let url = format!(
                "https://api.smartrecruiters.com/v1/companies/{slug}/postings?limit={PAGE_LIMIT}&offset={offset}"
            );
            let resp = client.get(&url).send().await.with_context(|| format!("GET {url}"))?;
            if resp.status().as_u16() == 404 {
                if offset == 0 {
                    anyhow::bail!("404");
                }
                break;
            }
            let resp = resp.error_for_status()?;
            let page: SrPage = resp.json().await.context("parse smartrecruiters page")?;
            debug!(slug, offset, got = page.content.len(), total = page.total_found, "page");
            if page.content.is_empty() {
                break;
            }
            for entry in page.content {
                let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
                let p: SrPosting = match serde_json::from_value(entry) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let apply_url = format!("https://jobs.smartrecruiters.com/{slug}/{}", p.id);
                let location = p.location.as_ref().and_then(|l| l.full_location.clone());
                let remote = p.location.as_ref().and_then(|l| l.remote);
                out.push(AdapterJob {
                    external_id: p.id,
                    title: p.name,
                    location,
                    department: p.department.and_then(|d| d.label),
                    apply_url,
                    description: None,
                    remote,
                    posted_at: p.released_date,
                    raw_json,
                });
            }
            offset += PAGE_LIMIT;
            if offset >= page.total_found {
                break;
            }
            tokio::time::sleep(PAGE_DELAY).await;
        }
        Ok(out)
    }
}
