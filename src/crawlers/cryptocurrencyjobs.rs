use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{debug, info, warn};
use url::Url;

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const RSS_URL: &str = "https://cryptocurrencyjobs.co/index.xml";
const SOURCE: &str = "cryptocurrencyjobs";
const POLITENESS_DELAY: Duration = Duration::from_millis(250);
const APPLY_REF_MARKER: &str = "ref=cryptocurrencyjobs.co";

pub struct CryptocurrencyJobs;

#[async_trait(?Send)]
impl Crawler for CryptocurrencyJobs {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = RSS_URL, "fetching RSS");
        let resp = client.get(RSS_URL).send().await.context("GET RSS")?;
        let status = resp.status();
        let body = resp.text().await.context("read RSS body")?;
        info!(status = status.as_u16(), bytes = body.len(), "RSS fetched");

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }

        let items = parse_rss_items(&body);
        info!(count = items.len(), "RSS items parsed");

        for (idx, item) in items.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            report.pages_visited += 1;

            let apply_url = match fetch_apply_url(&client, &item.link).await {
                Ok(Some(u)) => u,
                Ok(None) => {
                    debug!(detail = %item.link, "no employer URL found");
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, detail = %item.link, "detail fetch failed");
                    continue;
                }
            };
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&apply_url) else {
                debug!(apply = %apply_url, "apply URL skipped (auth wall or bad URL)");
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) = match db.upsert_company(
                item.company_name.as_deref(),
                ats.kind,
                &ats.slug,
                SOURCE,
                Some(&item.link),
            ) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, slug = %ats.slug, "company upsert failed");
                    continue;
                }
            };
            if company_is_new {
                report.companies_new += 1;
                info!(
                    name = item.company_name.as_deref().unwrap_or(&ats.slug),
                    ats = ats.kind.as_str(),
                    slug = %ats.slug,
                    "new company"
                );
            }

            let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title: &item.title,
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
                        info!(title = %item.title, ats = ats.kind.as_str(), "new job");
                    }
                }
                Err(e) => warn!(error = %e, title = %item.title, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

#[derive(Debug)]
struct RssItem {
    title: String,
    company_name: Option<String>,
    link: String,
}

fn parse_rss_items(xml: &str) -> Vec<RssItem> {
    static ITEM_RE: OnceLock<Regex> = OnceLock::new();
    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    static LINK_RE: OnceLock<Regex> = OnceLock::new();
    let item_re = ITEM_RE.get_or_init(|| Regex::new(r"(?s)<item>(.*?)</item>").unwrap());
    let title_re = TITLE_RE.get_or_init(|| Regex::new(r"(?s)<title>(.*?)</title>").unwrap());
    let link_re = LINK_RE.get_or_init(|| Regex::new(r"(?s)<link>(.*?)</link>").unwrap());

    let mut out = Vec::new();
    for cap in item_re.captures_iter(xml) {
        let block = &cap[1];
        let raw_title = title_re.captures(block).map(|c| c[1].trim().to_string());
        let link = link_re.captures(block).map(|c| c[1].trim().to_string());
        let (Some(raw_title), Some(link)) = (raw_title, link) else {
            continue;
        };
        let decoded = decode_basic_entities(&raw_title);
        let (title, company_name) = split_title_at_company(&decoded);
        out.push(RssItem {
            title,
            company_name,
            link,
        });
    }
    out
}

/// Titles are formatted "Senior Customer Success Manager - Fintech at BitGo".
/// Split on the last ` at ` so we get role + company.
fn split_title_at_company(title: &str) -> (String, Option<String>) {
    if let Some(idx) = title.rfind(" at ") {
        let role = title[..idx].trim().to_string();
        let company = title[idx + 4..].trim().to_string();
        if !role.is_empty() && !company.is_empty() {
            return (role, Some(company));
        }
    }
    (title.to_string(), None)
}

fn decode_basic_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
}

/// On a cryptocurrencyjobs.co detail page, the employer's apply URL is the
/// first outbound href that carries `?ref=cryptocurrencyjobs.co`. If that
/// marker isn't present, fall back to the first non-self, non-social URL.
async fn fetch_apply_url(client: &Client, detail_url: &str) -> Result<Option<String>> {
    let resp = client.get(detail_url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let doc = Html::parse_document(&body);
    let a = Selector::parse("a[href]").expect("static selector");

    let is_external = |href: &str| -> bool {
        let Ok(u) = Url::parse(href) else {
            return false;
        };
        let Some(h) = u.host_str() else { return false };
        !h.ends_with("cryptocurrencyjobs.co")
    };

    let is_social_or_util = |host: &str| -> bool {
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
        )
    };

    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        if href.contains(APPLY_REF_MARKER) && is_external(href) {
            return Ok(Some(href.to_string()));
        }
    }

    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        if !is_external(href) {
            continue;
        }
        let Ok(u) = Url::parse(href) else { continue };
        let Some(host) = u.host_str() else { continue };
        if is_social_or_util(host) {
            continue;
        }
        return Ok(Some(href.to_string()));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_parse_single_item() {
        let xml = r#"<rss><channel>
<item>
<title>Senior Customer Success Manager - Fintech at BitGo</title>
<link>https://cryptocurrencyjobs.co/finance/bitgo-x/</link>
</item>
</channel></rss>"#;
        let items = parse_rss_items(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Senior Customer Success Manager - Fintech");
        assert_eq!(items[0].company_name.as_deref(), Some("BitGo"));
        assert_eq!(
            items[0].link,
            "https://cryptocurrencyjobs.co/finance/bitgo-x/"
        );
    }

    #[test]
    fn title_without_at_clause() {
        let (t, c) = split_title_at_company("Just a role");
        assert_eq!(t, "Just a role");
        assert!(c.is_none());
    }
}
