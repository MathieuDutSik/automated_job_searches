use anyhow::{Context, Result};
use async_trait::async_trait;
use scraper::{Html, Selector};
use std::collections::HashSet;
use tracing::{debug, info, warn};

use crate::ats::classify_apply_url;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::Db;
use crate::http;

const URL: &str = "https://web3.career/remote-jobs";
const SOURCE: &str = "web3career";

pub struct Web3Career;

#[async_trait(?Send)]
impl Crawler for Web3Career {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = URL, "fetching");
        let resp = client.get(URL).send().await.context("GET web3.career")?;
        let status = resp.status();
        let body = resp.text().await.context("read body")?;
        info!(status = status.as_u16(), bytes = body.len(), "fetched");

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            warn!(status = status.as_u16(), "non-2xx, aborting parse");
            return Ok(report);
        }

        let links = extract_links(&body);
        report.links_examined = links.len() as u64;

        for (name, url) in links {
            let Some(ats) = classify_apply_url(&url) else {
                debug!(url = %url, "unrecognized ATS, skipping");
                continue;
            };
            report.companies_matched += 1;
            match db.upsert_company(&name, ats.kind, &ats.slug, SOURCE, Some(&url)) {
                Ok((_, is_new)) => {
                    if is_new {
                        report.companies_new += 1;
                        info!(name = %name, ats = ats.kind.as_str(), slug = %ats.slug, "new company");
                    }
                }
                Err(e) => warn!(error = %e, name = %name, "upsert failed"),
            }
        }

        Ok(report)
    }
}

/// Walk every <a href> on the page and return (anchor_text, normalized_href).
/// The classifier decides which of those are ATS URLs worth keeping.
fn extract_links(html: &str) -> Vec<(String, String)> {
    let doc = Html::parse_document(html);
    let a = Selector::parse("a[href]").expect("static selector");
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for el in doc.select(&a) {
        let Some(href) = el.value().attr("href") else { continue };
        let href = normalize(href);
        if !seen.insert(href.clone()) {
            continue;
        }
        let text: String = el.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
        out.push((text, href));
    }
    out
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
