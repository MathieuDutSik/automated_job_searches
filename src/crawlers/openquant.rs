use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const SITEMAP_URL: &str = "https://openquant.co/server-sitemap-index.xml";
const SOURCE: &str = "openquant";
/// robots.txt allows everything and sets no Crawl-delay; one page a second
/// is our own restraint.
const POLITENESS_DELAY: Duration = Duration::from_millis(1000);
/// The sitemap keeps every job ever posted (back to 2022) and its
/// `lastmod` is always "now". Ids grow over time, so pages are visited
/// newest first and the crawl stops after this many consecutive postings
/// older than MAX_AGE_DAYS.
const MAX_AGE_DAYS: i64 = 120;
const STOP_AFTER_OLD: usize = 25;

/// OpenQuant (openquant.co), a job board for quantitative research,
/// trading and development roles. The listing page's pagination is
/// client-side, so jobs come from the per-job pages listed in the server
/// sitemap; each embeds its record (`__NEXT_DATA__` → props.pageProps.data)
/// with the employer's own application URL — Workday, Greenhouse, ... —
/// which is what files the job and its company under the right ATS.
pub struct OpenQuant;

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct OqJob {
    #[serde(rename = "ID")]
    id: String,
    company_name: Option<String>,
    company_type: Option<String>,
    position: String,
    position_type: Option<String>,
    seniority: Option<String>,
    posted_date: Option<String>,
    location: Option<String>,
    country: Option<String>,
    application_url: Option<String>,
    description: Option<String>,
    skills: Option<String>,
}

#[async_trait(?Send)]
impl Crawler for OpenQuant {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = SITEMAP_URL, "fetching sitemap");
        let resp = client.get(SITEMAP_URL).send().await.context("GET openquant sitemap")?;
        let status = resp.status();
        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }
        let pages = job_pages_newest_first(&resp.text().await?);
        info!(count = pages.len(), "job pages in sitemap");

        let cutoff = (chrono::Utc::now() - chrono::Duration::days(MAX_AGE_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        let mut old_in_a_row = 0;
        for (idx, page) in pages.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            let job = match fetch_job(&client, page).await {
                Ok(j) => j,
                Err(e) => {
                    warn!(error = %e, url = %page, "job page failed");
                    continue;
                }
            };
            report.pages_visited += 1;
            if job.posted_date.as_deref().is_some_and(|d| d < cutoff.as_str()) {
                old_in_a_row += 1;
                if old_in_a_row >= STOP_AFTER_OLD {
                    info!(visited = report.pages_visited, cutoff, "reached old postings; stopping");
                    break;
                }
                continue;
            }
            old_in_a_row = 0;
            store_job(db, job, page, &mut report);
        }
        Ok(report)
    }
}

/// `/job/{slug}/{id}` URLs from the sitemap, highest id first.
fn job_pages_newest_first(sitemap: &str) -> Vec<String> {
    static LOC: OnceLock<Regex> = OnceLock::new();
    let re = LOC.get_or_init(|| {
        Regex::new(r"<loc>\s*(https://openquant\.co/job/[^<\s]+/(\d+))\s*</loc>").unwrap()
    });
    let mut pages: Vec<(u64, String)> = re
        .captures_iter(sitemap)
        .filter_map(|c| Some((c[2].parse().ok()?, c[1].to_string())))
        .collect();
    pages.sort_by(|a, b| b.0.cmp(&a.0));
    pages.into_iter().map(|(_, u)| u).collect()
}

async fn fetch_job(client: &reqwest::Client, url: &str) -> Result<OqJob> {
    let html = client.get(url).send().await?.error_for_status()?.text().await?;
    parse_job_page(&html)
}

fn parse_job_page(html: &str) -> Result<OqJob> {
    static NEXT: OnceLock<Regex> = OnceLock::new();
    let re = NEXT.get_or_init(|| {
        Regex::new(r#"(?s)<script id="__NEXT_DATA__" type="application/json">(.*?)</script>"#)
            .unwrap()
    });
    let json = re.captures(html).context("no __NEXT_DATA__")?;
    let value: serde_json::Value = serde_json::from_str(&json[1])?;
    let data = value
        .pointer("/props/pageProps/data")
        .context("no props.pageProps.data")?
        .clone();
    Ok(serde_json::from_value(data)?)
}

fn store_job(db: &Db, job: OqJob, page: &str, report: &mut CrawlReport) {
    let Some(apply_url) = job.application_url.clone().filter(|u| u.starts_with("http")) else {
        return;
    };
    report.apply_links_found += 1;
    let Some(ats) = classify_or_other(&apply_url) else {
        return;
    };
    report.jobs_matched += 1;
    let (company_id, is_new) = match db.upsert_company(
        job.company_name.as_deref(),
        ats.kind,
        &ats.slug,
        SOURCE,
        Some(page),
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, slug = %ats.slug, "company upsert failed");
            return;
        }
    };
    if is_new {
        report.companies_new += 1;
    }
    let location = [&job.location, &job.country]
        .into_iter()
        .flatten()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
    let raw_json = serde_json::to_string(&job).unwrap_or_else(|_| "{}".to_string());
    match db.upsert_job(JobUpsert {
        source: SOURCE,
        company_id,
        kind: ats.kind,
        external_id: &external_id,
        title: job.position.trim(),
        location: Some(location.as_str()).filter(|s| !s.is_empty()),
        department: job.position_type.as_deref(),
        apply_url: &apply_url,
        description: job.description.as_deref(),
        // OpenQuant has no remote flag; the filter falls back to the
        // location text.
        remote: None,
        posted_at: job.posted_date.as_deref(),
        raw_json: &raw_json,
    }) {
        Ok((_, is_new)) => {
            if is_new {
                report.jobs_new += 1;
            }
        }
        Err(e) => warn!(error = %e, title = %job.position, "job upsert failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sitemap_sorted_newest_first() {
        let xml = "<urlset>\
            <url><loc>https://openquant.co/job/a-optiver/5</loc></url>\
            <url><loc>https://openquant.co/job/b-drw/4016</loc></url>\
            <url><loc>https://openquant.co/job/c-imc/310</loc></url>\
            </urlset>";
        assert_eq!(
            job_pages_newest_first(xml),
            vec![
                "https://openquant.co/job/b-drw/4016",
                "https://openquant.co/job/c-imc/310",
                "https://openquant.co/job/a-optiver/5",
            ]
        );
    }

    #[test]
    fn parses_job_page() {
        let html = r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"data":{"ID":"x","CompanyName":"Deutsche Bank","Position":"Quantitative Trader","PostedDate":"2026-11-06","Location":"New York, US","Country":"United States","ApplicationUrl":"https://db.wd3.myworkdayjobs.com/DBWebsite/job/NY/Quantitative-Trader_R1","MinSalary":0}}}}</script>"#;
        let j = parse_job_page(html).unwrap();
        assert_eq!(j.company_name.as_deref(), Some("Deutsche Bank"));
        assert_eq!(j.posted_date.as_deref(), Some("2026-11-06"));
        assert!(j.application_url.unwrap().contains("myworkdayjobs.com"));
    }
}
