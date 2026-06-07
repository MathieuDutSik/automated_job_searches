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

const LIST_URL: &str = "https://thehub.io/jobs";
const SOURCE: &str = "thehub";
const HOST: &str = "thehub.io";
const POLITENESS_DELAY: Duration = Duration::from_millis(250);

pub struct TheHub;

#[async_trait(?Send)]
impl Crawler for TheHub {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = LIST_URL, "fetching listing");
        let resp = client
            .get(LIST_URL)
            .send()
            .await
            .context("GET thehub listing")?;
        let status = resp.status();
        let body = resp.text().await.context("read body")?;
        info!(
            status = status.as_u16(),
            bytes = body.len(),
            "listing fetched"
        );

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }

        let detail_paths = parse_listing(&body);
        info!(
            count = detail_paths.len(),
            "detail URLs extracted (page 1 only; rest of pagination is JS-driven)"
        );

        for (idx, path) in detail_paths.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            report.pages_visited += 1;
            let detail_url = format!("https://{HOST}{path}");

            let (title, apply_url) = match fetch_detail(&client, &detail_url).await {
                Ok(Some(pair)) => pair,
                Ok(None) => {
                    debug!(detail = %detail_url, "no outbound apply URL found");
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, detail = %detail_url, "detail fetch failed");
                    continue;
                }
            };
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&apply_url) else {
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) =
                match db.upsert_company(None, ats.kind, &ats.slug, SOURCE, Some(&detail_url)) {
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
            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title: &title,
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
                    }
                }
                Err(e) => warn!(error = %e, title = %title, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

/// Extract de-duped detail-page paths (`/jobs/{hex}`) from the listing HTML.
fn parse_listing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let a = Selector::parse(r#"a[href^="/jobs/"]"#).expect("static selector");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        // Match `/jobs/{24-hex}` exactly — skip filter/category pages.
        let tail = href.trim_start_matches("/jobs/");
        if tail.len() < 12 || !tail.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if seen.insert(href.to_string()) {
            out.push(href.to_string());
        }
    }
    out
}

/// Fetch a thehub.io detail page and pull out the role title + first outbound
/// apply URL. Strategy: pick the first non-self anchor that is NOT social
/// media; thehub.io tends to put the "Apply" button as an external link to
/// the company's careers page or ATS posting.
async fn fetch_detail(client: &Client, detail_url: &str) -> Result<Option<(String, String)>> {
    let resp = client.get(detail_url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let doc = Html::parse_document(&body);

    // Title: prefer <h1>, fall back to <title>.
    let h1_sel = Selector::parse("h1").expect("static selector");
    let title_sel = Selector::parse("title").expect("static selector");
    let title = doc
        .select(&h1_sel)
        .next()
        .map(|e| e.text().collect::<String>())
        .or_else(|| {
            doc.select(&title_sel)
                .next()
                .map(|e| e.text().collect::<String>())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| detail_url.to_string());

    let a_sel = Selector::parse("a[href]").expect("static selector");
    for el in doc.select(&a_sel) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let Ok(u) = Url::parse(href) else { continue };
        let Some(host) = u.host_str() else { continue };
        if host.ends_with(HOST) {
            continue;
        }
        if is_social_or_util(host) {
            continue;
        }
        return Ok(Some((title, href.to_string())));
    }
    Ok(None)
}

fn is_social_or_util(host: &str) -> bool {
    matches!(
        host,
        "facebook.com"
            | "www.facebook.com"
            | "x.com"
            | "twitter.com"
            | "www.twitter.com"
            | "linkedin.com"
            | "www.linkedin.com"
            | "youtube.com"
            | "www.youtube.com"
            | "instagram.com"
            | "www.instagram.com"
            | "tiktok.com"
            | "www.tiktok.com"
            | "github.com"
            | "google.com"
            | "medium.com"
            | "discord.com"
            | "discord.gg"
            | "slack.com"
            | "t.me"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_extracts_hex_ids_dedup() {
        let html = r#"
            <a href="/jobs/aaaaaaaaaaaaaaaaaaaaaaaa">A</a>
            <a href="/jobs/aaaaaaaaaaaaaaaaaaaaaaaa">A again</a>
            <a href="/jobs/bbbbbbbbbbbbbbbbbbbbbbbb">B</a>
            <a href="/jobs/filter">skip</a>
            <a href="/jobs/category/engineering">skip nested</a>
        "#;
        let paths = parse_listing(html);
        assert_eq!(paths.len(), 2);
    }
}
