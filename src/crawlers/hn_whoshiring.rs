use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

use crate::ats::{classify_or_other, AtsKind};
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const SOURCE: &str = "hn_whoshiring";
const SEARCH_URL: &str = "https://hn.algolia.com/api/v1/search_by_date?query=Ask+HN+Who+is+hiring&tags=story&hitsPerPage=10";
const ITEM_URL_FMT: &str = "https://hn.algolia.com/api/v1/items/";

pub struct HnWhosHiring;

#[derive(Deserialize, Debug)]
struct SearchResp {
    hits: Vec<SearchHit>,
}

#[derive(Deserialize, Debug)]
struct SearchHit {
    #[serde(rename = "objectID")]
    object_id: String,
    title: Option<String>,
}

#[derive(Deserialize, Debug)]
struct Item {
    #[allow(dead_code)]
    id: u64,
    text: Option<String>,
    #[serde(default)]
    children: Vec<Item>,
}

#[async_trait(?Send)]
impl Crawler for HnWhosHiring {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        let thread_id = find_latest_thread(&client).await?;
        info!(thread_id = %thread_id, "found latest Who Is Hiring thread");

        let item_url = format!("{ITEM_URL_FMT}{thread_id}");
        info!(url = %item_url, "fetching thread items");
        let resp = client.get(&item_url).send().await.context("GET thread")?;
        let status = resp.status();
        let thread: Item = resp.json().await.context("parse thread JSON")?;

        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        report.pages_visited = 1;

        info!(comments = thread.children.len(), "top-level comments");
        for child in &thread.children {
            let Some(text) = &child.text else { continue };
            let title = extract_title(text);
            let company_hint = extract_company_hint(&title);
            let urls = extract_urls(text);

            // Prefer a known ATS over a generic Other classification.
            let mut chosen: Option<(String, crate::ats::AtsRef)> = None;
            for u in urls {
                let Some(r) = classify_or_other(&u) else { continue };
                let promote = match (&chosen, r.kind) {
                    (None, _) => true,
                    (Some((_, prev)), new) if prev.kind == AtsKind::Other && new != AtsKind::Other => true,
                    _ => false,
                };
                if promote {
                    chosen = Some((u, r));
                }
            }

            let Some((apply_url, ats)) = chosen else {
                debug!(title = %title, "no usable URL in comment");
                continue;
            };
            report.apply_links_found += 1;
            report.jobs_matched += 1;

            let (company_id, company_is_new) = match db.upsert_company(
                company_hint.as_deref(),
                ats.kind,
                &ats.slug,
                SOURCE,
                Some(&apply_url),
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
                Err(e) => warn!(error = %e, "job upsert failed"),
            }
        }

        Ok(report)
    }
}

async fn find_latest_thread(client: &Client) -> Result<String> {
    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let title_re =
        TITLE_RE.get_or_init(|| Regex::new(r"(?i)Ask HN:?\s*Who is hiring\?").unwrap());

    let resp: SearchResp = client.get(SEARCH_URL).send().await?.json().await?;
    for hit in resp.hits {
        if let Some(title) = &hit.title {
            if title_re.is_match(title) {
                return Ok(hit.object_id);
            }
        }
    }
    anyhow::bail!("no 'Ask HN: Who is hiring?' thread found in search results");
}

fn extract_title(html_text: &str) -> String {
    let first_para = html_text.split("<p>").next().unwrap_or(html_text);
    let no_tags = strip_tags(first_para);
    let decoded = decode_basic_entities(&no_tags);
    decoded
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn extract_company_hint(title: &str) -> Option<String> {
    let first = title.split('|').next()?.trim();
    if first.is_empty() || first.len() > 80 {
        None
    } else {
        Some(first.to_string())
    }
}

fn extract_urls(text: &str) -> Vec<String> {
    static HREF_RE: OnceLock<Regex> = OnceLock::new();
    let href_re = HREF_RE.get_or_init(|| Regex::new(r#"href="([^"]+)""#).unwrap());
    href_re
        .captures_iter(text)
        .map(|c| decode_basic_entities(&c[1]))
        .collect()
}

fn strip_tags(s: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    tag_re.replace_all(s, " ").into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_urls_from_hn_text() {
        let text = r#"Acme | SWE | Remote | <a href="https:&#x2F;&#x2F;boards.greenhouse.io&#x2F;acme&#x2F;jobs&#x2F;1" rel="nofollow">apply</a><p>desc"#;
        let urls = extract_urls(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://boards.greenhouse.io/acme/jobs/1");
    }

    #[test]
    fn extract_title_strips_html() {
        let text = r#"Acme | SWE | Remote | <a href="x">link</a><p>longer description"#;
        let t = extract_title(text);
        assert!(t.starts_with("Acme |"));
        assert!(!t.contains("<"));
    }

    #[test]
    fn company_hint_from_title() {
        assert_eq!(extract_company_hint("Acme | SWE | Remote"), Some("Acme".to_string()));
        assert_eq!(
            extract_company_hint("Adacore | Software Engineers | Full-time | Remote"),
            Some("Adacore".to_string())
        );
    }
}
