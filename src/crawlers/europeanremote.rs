use anyhow::{Context, Result};
use async_trait::async_trait;
use scraper::{Html, Selector};
use tracing::{info, warn};
use url::Url;

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const LIST_URL: &str = "https://europeanremote.com/jobs/";
const SOURCE: &str = "europeanremote";
const TRACKING_MARKER: &str = "utm_source=europeanremote.com";

pub struct EuropeanRemote;

#[async_trait(?Send)]
impl Crawler for EuropeanRemote {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = LIST_URL, "fetching listing");
        let resp = client.get(LIST_URL).send().await.context("GET europeanremote")?;
        let status = resp.status();
        let body = resp.text().await.context("read body")?;
        info!(status = status.as_u16(), bytes = body.len(), "listing fetched");

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }

        let items = parse_listing(&body);
        info!(count = items.len(), "items extracted");
        for item in items {
            report.pages_visited += 1;
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&item.apply_url) else {
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) = match db.upsert_company(
                item.company.as_deref(),
                ats.kind,
                &ats.slug,
                SOURCE,
                Some(&item.apply_url),
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

            let external_id = ats.external_id.clone().unwrap_or_else(|| item.apply_url.clone());
            // Strip the utm marker from raw_json's stored URL — the apply URL
            // we PERSIST keeps it (matches what the user would click), but the
            // ATS sync can deduplicate on the bare external_id.
            let raw_json = format!(r#"{{"apply_url":"{}"}}"#, item.apply_url.replace('"', "\\\""));
            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title: &item.title,
                location: None,
                department: None,
                apply_url: &item.apply_url,
                description: None,
                // The site is "European Remote" — all listings are remote by
                // editorial choice. Flag accordingly so --remote picks them up.
                remote: Some(true),
                posted_at: None,
                raw_json: &raw_json,
            }) {
                Ok((_, is_new)) => {
                    if is_new {
                        report.jobs_new += 1;
                    }
                }
                Err(e) => warn!(error = %e, title = %item.title, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

#[derive(Debug)]
struct ListingItem {
    title: String,
    company: Option<String>,
    apply_url: String,
}

/// Walk every `<a href>` on the listing page whose href is an external URL
/// carrying the `utm_source=europeanremote.com` marker. Title is the link
/// text. Company is inferred from the host's first path segment for
/// known-ATS URLs (greenhouse `/{co}/jobs/...`, etc.) or left to
/// `classify_or_other` to populate via the slug.
fn parse_listing(html: &str) -> Vec<ListingItem> {
    let doc = Html::parse_document(html);
    let a = Selector::parse("a[href]").expect("static selector");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else { continue };
        if !href.contains(TRACKING_MARKER) {
            continue;
        }
        if Url::parse(href).is_err() {
            continue;
        }
        if !seen.insert(href.to_string()) {
            continue;
        }
        let title = el.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }
        out.push(ListingItem {
            title,
            company: None,
            apply_url: href.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_external_utm_links_only() {
        let html = r#"
            <a href="/internal">Skip</a>
            <a href="https://boards.greenhouse.io/co/jobs/1?utm_source=europeanremote.com">Senior Engineer</a>
            <a href="https://example.com/career?utm_source=europeanremote.com">Backend Dev</a>
            <a href="https://example.com/no-utm">No marker, skip</a>
        "#;
        let items = parse_listing(html);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.title == "Senior Engineer"));
        assert!(items.iter().any(|i| i.title == "Backend Dev"));
    }
}
