use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tracing::debug;

use crate::adapters::{AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Workday;

const PAGE_LIMIT: u32 = 20;
const PAGE_DELAY: Duration = Duration::from_millis(150);

#[derive(Deserialize, Debug)]
struct WdPage {
    #[serde(default)]
    total: u32,
    #[serde(default, rename = "jobPostings")]
    job_postings: Vec<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct WdPosting {
    title: String,
    #[serde(rename = "externalPath")]
    external_path: String,
    #[serde(default, rename = "locationsText")]
    locations_text: Option<String>,
    #[serde(default, rename = "postedOn")]
    posted_on: Option<String>,
    #[serde(default, rename = "bulletFields")]
    bullet_fields: Vec<String>,
}

/// Split the composite slug `tenant/wd{N}/site` back into its parts.
fn split_slug(slug: &str) -> Option<(&str, &str, &str)> {
    let mut it = slug.splitn(3, '/');
    let tenant = it.next()?;
    let region = it.next()?;
    let site = it.next()?;
    if tenant.is_empty() || !region.starts_with("wd") || site.is_empty() {
        return None;
    }
    Some((tenant, region, site))
}

#[async_trait(?Send)]
impl AtsAdapter for Workday {
    fn kind(&self) -> AtsKind {
        AtsKind::Workday
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let Some((tenant, region, site)) = split_slug(slug) else {
            anyhow::bail!("workday slug must be `tenant/wd{{N}}/site` (got `{slug}`)");
        };
        let api_url =
            format!("https://{tenant}.{region}.myworkdayjobs.com/wday/cxs/{tenant}/{site}/jobs");
        let public_root = format!("https://{tenant}.{region}.myworkdayjobs.com/en-US/{site}");
        let client = http::client()?;

        let mut out: Vec<AdapterJob> = Vec::new();
        let mut offset: u32 = 0;
        // Workday returns `total` only on the first page; subsequent pages
        // report `total: 0`. Pin the figure from page 1 so we can still use
        // it as the upper bound for the loop.
        let mut known_total: u32 = u32::MAX;
        loop {
            let body = json!({
                "appliedFacets": {},
                "limit": PAGE_LIMIT,
                "offset": offset,
                "searchText": ""
            });
            let resp = client
                .post(&api_url)
                .json(&body)
                .send()
                .await
                .with_context(|| format!("POST {api_url}"))?;
            if resp.status().as_u16() == 404 {
                if offset == 0 {
                    anyhow::bail!("404");
                }
                break;
            }
            let resp = resp.error_for_status()?;
            let page: WdPage = resp.json().await.context("parse workday page")?;
            if offset == 0 && page.total > 0 {
                known_total = page.total;
            }
            debug!(
                slug,
                offset,
                got = page.job_postings.len(),
                total = known_total,
                "page"
            );
            if page.job_postings.is_empty() {
                break;
            }
            for entry in page.job_postings {
                let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
                let p: WdPosting = match serde_json::from_value(entry) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                // `externalPath` is `/job/{Location}/{Title}_{ExternalID}`.
                // Prefer bulletFields[0] for external_id (cleaner — usually
                // just the JR code), fall back to the tail of externalPath.
                let external_id = p
                    .bullet_fields
                    .first()
                    .cloned()
                    .or_else(|| p.external_path.rsplit('/').next().map(|s| s.to_string()))
                    .unwrap_or_else(|| p.external_path.clone());
                let apply_url = format!("{public_root}{}", p.external_path);
                let location = p.locations_text.clone();
                let remote = location.as_deref().map(|l| {
                    let lc = l.to_ascii_lowercase();
                    lc.contains("remote") || lc.contains("anywhere")
                });
                out.push(AdapterJob {
                    external_id,
                    title: p.title,
                    location,
                    department: None,
                    apply_url,
                    description: None,
                    remote,
                    posted_at: p.posted_on,
                    raw_json,
                });
            }
            offset += PAGE_LIMIT;
            if offset >= known_total {
                break;
            }
            tokio::time::sleep(PAGE_DELAY).await;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_slug_valid() {
        let (t, r, s) = split_slug("nvidia/wd5/NVIDIAExternalCareerSite").unwrap();
        assert_eq!(t, "nvidia");
        assert_eq!(r, "wd5");
        assert_eq!(s, "NVIDIAExternalCareerSite");
    }

    #[test]
    fn split_slug_rejects_bad_region() {
        assert!(split_slug("nvidia/foo/Site").is_none());
        assert!(split_slug("nvidia/wd5").is_none()); // missing site
        assert!(split_slug("").is_none());
    }
}
