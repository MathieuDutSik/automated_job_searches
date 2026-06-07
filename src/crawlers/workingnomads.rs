use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const API_URL: &str = "https://www.workingnomads.com/api/exposed_jobs/";
const SOURCE: &str = "workingnomads";
const POLITENESS_DELAY: Duration = Duration::from_millis(250);

pub struct WorkingNomads;

#[derive(Deserialize, Debug, serde::Serialize)]
struct WnJob {
    url: String,
    title: String,
    description: Option<String>,
    company_name: Option<String>,
    category_name: Option<String>,
    tags: Option<String>,
    location: Option<String>,
    pub_date: Option<String>,
}

#[async_trait(?Send)]
impl Crawler for WorkingNomads {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = API_URL, "fetching JSON");
        let resp = client.get(API_URL).send().await.context("GET workingnomads API")?;
        let status = resp.status();

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }

        let jobs: Vec<WnJob> = resp.json().await.context("parse workingnomads JSON")?;
        info!(count = jobs.len(), "JSON parsed");

        for (idx, job) in jobs.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            report.pages_visited += 1;

            // /job/go/{id}/ is a 302 to the real apply URL; follow once.
            let apply_url = match resolve_real_apply_url(&client, &job.url).await {
                Ok(u) => u,
                Err(e) => {
                    warn!(error = %e, url = %job.url, "redirect resolve failed");
                    continue;
                }
            };
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&apply_url) else {
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) = match db.upsert_company(
                job.company_name.as_deref(),
                ats.kind,
                &ats.slug,
                SOURCE,
                Some(&job.url),
            ) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, slug = %ats.slug, "company upsert failed");
                    continue;
                }
            };
            if company_is_new {
                report.companies_new += 1;
            }

            let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
            let raw_json = serde_json::to_string(&job).unwrap_or_else(|_| "{}".to_string());
            let description = job.description.as_deref().map(|d| strip_html_tags(d.to_string()));
            let remote = job
                .location
                .as_deref()
                .map(|l| looks_remote(l) || job.tags.as_deref().is_some_and(looks_remote));

            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title: &job.title,
                location: job.location.as_deref().filter(|s| !s.is_empty()),
                department: job.category_name.as_deref(),
                apply_url: &apply_url,
                description: description.as_deref(),
                remote,
                posted_at: job.pub_date.as_deref(),
                raw_json: &raw_json,
            }) {
                Ok((_, is_new)) => {
                    if is_new {
                        report.jobs_new += 1;
                    }
                }
                Err(e) => warn!(error = %e, title = %job.title, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

/// HEAD the workingnomads redirect URL and return the final URL it resolves to.
/// reqwest follows redirects by default, so `resp.url()` is the destination.
async fn resolve_real_apply_url(client: &reqwest::Client, redirect_url: &str) -> Result<String> {
    let resp = client.head(redirect_url).send().await?;
    Ok(resp.url().to_string())
}

fn looks_remote(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("remote") || l.contains("anywhere") || l.contains("worldwide")
}

fn strip_html_tags(s: String) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let no_tags = tag.replace_all(&s, " ");
    let decoded = no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_remote_basic() {
        assert!(looks_remote("Anywhere"));
        assert!(looks_remote("USA Remote"));
        assert!(looks_remote("Worldwide"));
        assert!(!looks_remote("Berlin, Germany"));
    }

    #[test]
    fn strip_tags_decodes_entities_and_collapses() {
        let out = strip_html_tags("<p>Hello&nbsp;&amp; world</p>".to_string());
        assert_eq!(out, "Hello & world");
    }
}
