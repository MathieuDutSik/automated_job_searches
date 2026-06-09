use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::Duration;
use tracing::{info, warn};

use crate::ats::AtsKind;
use crate::db::{Db, JobUpsert};

pub mod ashby;
pub mod bamboohr;
pub mod greenhouse;
pub mod lever;
pub mod recruitee;
pub mod smartrecruiters;
pub mod workday;

pub const POLITENESS_DELAY: Duration = Duration::from_millis(250);

pub struct AdapterJob {
    pub external_id: String,
    pub title: String,
    pub location: Option<String>,
    pub department: Option<String>,
    pub apply_url: String,
    pub description: Option<String>,
    pub remote: Option<bool>,
    pub posted_at: Option<String>,
    pub raw_json: String,
}

#[async_trait(?Send)]
pub trait AtsAdapter {
    fn kind(&self) -> AtsKind;
    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>>;
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub kind: &'static str,
    pub companies_synced: u64,
    pub companies_404: u64,
    pub jobs_seen: u64,
    pub jobs_new: u64,
    pub jobs_closed: u64,
}

pub fn all() -> Vec<Box<dyn AtsAdapter>> {
    vec![
        Box::new(greenhouse::Greenhouse),
        Box::new(ashby::Ashby),
        Box::new(lever::Lever),
        Box::new(smartrecruiters::SmartRecruiters),
        Box::new(bamboohr::BambooHr),
        Box::new(recruitee::Recruitee),
        Box::new(workday::Workday),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn AtsAdapter>> {
    all().into_iter().find(|a| a.kind().as_str() == name)
}

/// Sync a single (company_id, slug) pair through `adapter`. Returns a
/// per-call `SyncReport` the caller aggregates as needed. `display_name`
/// is purely for logging — pass the slug when no human-readable name is
/// available.
pub async fn sync_one_slug(
    db: &Db,
    adapter: &dyn AtsAdapter,
    company_id: i64,
    display_name: &str,
    slug: &str,
) -> Result<SyncReport> {
    let mut report = SyncReport {
        kind: adapter.kind().as_str(),
        ..Default::default()
    };
    let started = chrono::Utc::now().to_rfc3339();
    match adapter.fetch_jobs(slug).await {
        Ok(jobs) => {
            report.companies_synced = 1;
            let mut new_here = 0u64;
            for j in &jobs {
                let trimmed_title = j.title.trim();
                let trimmed_loc = j.location.as_deref().map(str::trim);
                let trimmed_dept = j.department.as_deref().map(str::trim);
                let trimmed_desc = j.description.as_deref().map(str::trim);
                let res = db.upsert_job(JobUpsert {
                    company_id,
                    kind: adapter.kind(),
                    external_id: &j.external_id,
                    title: trimmed_title,
                    location: trimmed_loc.filter(|s| !s.is_empty()),
                    department: trimmed_dept.filter(|s| !s.is_empty()),
                    apply_url: &j.apply_url,
                    description: trimmed_desc.filter(|s| !s.is_empty()),
                    remote: j.remote,
                    posted_at: j.posted_at.as_deref(),
                    raw_json: &j.raw_json,
                });
                match res {
                    Ok((_, is_new)) => {
                        report.jobs_seen += 1;
                        if is_new {
                            report.jobs_new += 1;
                            new_here += 1;
                        }
                    }
                    Err(e) => warn!(error = %e, slug, "job upsert failed"),
                }
            }
            let closed = db
                .mark_unseen_jobs_closed(company_id, adapter.kind(), &started)
                .unwrap_or(0);
            report.jobs_closed = closed as u64;
            info!(
                company = %display_name,
                slug,
                jobs = jobs.len(),
                new = new_here,
                closed,
                "synced"
            );
        }
        Err(e) => {
            if e.to_string().contains("404") {
                report.companies_404 = 1;
                warn!(slug, "ATS returned 404; slug may be stale");
            } else {
                warn!(error = %e, slug, "fetch failed");
            }
        }
    }
    Ok(report)
}

/// Sync every company in the DB whose `ats_kind` matches `adapter.kind()`.
pub async fn sync_all_for_kind(db: &Db, adapter: &dyn AtsAdapter) -> Result<SyncReport> {
    let mut report = SyncReport {
        kind: adapter.kind().as_str(),
        ..Default::default()
    };
    let slugs = db.list_slugs_for_kind(adapter.kind())?;
    info!(
        kind = adapter.kind().as_str(),
        count = slugs.len(),
        "starting sync"
    );
    for (idx, (company_id, name, slug)) in slugs.into_iter().enumerate() {
        if idx > 0 {
            tokio::time::sleep(POLITENESS_DELAY).await;
        }
        let r = sync_one_slug(db, adapter, company_id, &name, &slug).await?;
        report.companies_synced += r.companies_synced;
        report.companies_404 += r.companies_404;
        report.jobs_seen += r.jobs_seen;
        report.jobs_new += r.jobs_new;
        report.jobs_closed += r.jobs_closed;
    }
    Ok(report)
}

/// Fetch a URL as a generic JSON `Value`. Adapters use this so they can
/// preserve the original per-job JSON in `raw_json` alongside a typed view.
pub(crate) async fn fetch_value_or_none_on_404(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<serde_json::Value>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    let resp = resp.error_for_status()?;
    let body: serde_json::Value = resp.json().await.context("parse JSON")?;
    Ok(Some(body))
}

/// Best-effort HTML → plain text. Decodes a handful of common entities, then
/// uses scraper to drop tags, then collapses whitespace. Intended for ATS
/// description blobs (Greenhouse encodes its HTML with entities).
pub(crate) fn html_to_text(html: &str) -> String {
    let decoded = html
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");
    let frag = scraper::Html::parse_fragment(&decoded);
    let text = frag.root_element().text().collect::<Vec<_>>().join(" ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
