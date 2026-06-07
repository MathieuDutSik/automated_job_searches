use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use scraper::{Html, Selector};
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, info, warn};
use url::Url;

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const LIST_URL: &str = "https://web3.career/remote-jobs";
const SOURCE: &str = "web3career";
const POLITENESS_DELAY: Duration = Duration::from_millis(250);

pub struct Web3Career;

#[async_trait(?Send)]
impl Crawler for Web3Career {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = LIST_URL, "fetching listing");
        let resp = client.get(LIST_URL).send().await.context("GET listing")?;
        let status = resp.status();
        let body = resp.text().await.context("read listing body")?;
        info!(status = status.as_u16(), bytes = body.len(), "listing fetched");

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            warn!(status = status.as_u16(), "non-2xx, aborting");
            return Ok(report);
        }

        let detail_links = extract_detail_links(&body);
        info!(count = detail_links.len(), "job-detail URLs on listing");

        for (idx, (title, detail_url)) in detail_links.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            report.pages_visited += 1;

            let apply_url = match fetch_apply_url(&client, detail_url).await {
                Ok(Some(u)) => u,
                Ok(None) => {
                    debug!(detail = %detail_url, "no apply link found on detail page");
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, detail = %detail_url, "detail fetch failed");
                    continue;
                }
            };
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&apply_url) else {
                debug!(apply = %apply_url, "apply URL has no host, skipping");
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) =
                match db.upsert_company(None, ats.kind, &ats.slug, SOURCE, Some(detail_url)) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, slug = %ats.slug, "company upsert failed");
                        continue;
                    }
                };
            if company_is_new {
                report.companies_new += 1;
                info!(ats = ats.kind.as_str(), slug = %ats.slug, "new company");
            }

            let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title,
                location: None,
                department: None,
                apply_url: &apply_url,
                description: None,
                remote: None,
                posted_at: None,
                raw_json: "{}",
            }) {
                Ok((_, is_new)) => {
                    if is_new {
                        report.jobs_new += 1;
                        info!(title = %title, ats = ats.kind.as_str(), slug = %ats.slug, "new job");
                    }
                }
                Err(e) => warn!(error = %e, title = %title, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

/// Find job-detail URLs on the listing page. web3.career uses the pattern
/// `https://web3.career/<slug>/<numeric-id>` for individual job pages.
fn extract_detail_links(html: &str) -> Vec<(String, String)> {
    let doc = Html::parse_document(html);
    let a = Selector::parse("a[href]").expect("static selector");
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else { continue };
        let url = normalize(href);
        let Ok(parsed) = Url::parse(&url) else { continue };
        let host = parsed.host_str().unwrap_or("");
        if host != "web3.career" && host != "www.web3.career" {
            continue;
        }
        let segs: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();
        if segs.len() < 2 {
            continue;
        }
        let last = segs.last().unwrap();
        if !last.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        let text: String = el
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let title = if text.is_empty() {
            segs[segs.len() - 2].replace('-', " ")
        } else {
            text
        };
        out.push((title, url));
    }
    out
}

/// Fetch a job-detail page and return the URL its "Apply" button points to.
/// web3.career tags the Apply button URL with `?source=web3.career`, which is
/// a very strong signal. If that's missing (rare), fall back to an external
/// anchor whose visible text is exactly "apply now" / "apply".
async fn fetch_apply_url(client: &Client, detail_url: &str) -> Result<Option<String>> {
    let resp = client.get(detail_url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let doc = Html::parse_document(&body);
    let a = Selector::parse("a[href]").expect("static selector");

    let is_external = |href: &str| -> bool {
        let Ok(u) = Url::parse(href) else { return false };
        let Some(h) = u.host_str() else { return false };
        !h.ends_with("web3.career")
    };

    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else { continue };
        if href.contains("source=web3.career") && is_external(href) {
            return Ok(Some(href.to_string()));
        }
    }

    for el in doc.select(&a) {
        let text = el
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if text != "apply" && text != "apply now" {
            continue;
        }
        let Some(href) = el.value().attr("href") else { continue };
        if is_external(href) {
            return Ok(Some(href.to_string()));
        }
    }
    Ok(None)
}

fn normalize(href: &str) -> String {
    let trimmed = href.trim();
    if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else if trimmed.starts_with('/') {
        format!("https://web3.career{trimmed}")
    } else {
        trimmed.to_string()
    }
}
