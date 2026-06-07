use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use std::sync::OnceLock;
use tracing::{info, warn};

use crate::ats::classify_or_other;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const RSS_URL: &str = "https://www.cryptojobs.com/jobs/feed";
const SOURCE: &str = "cryptojobs";

pub struct CryptoJobs;

#[async_trait(?Send)]
impl Crawler for CryptoJobs {
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

        for item in items.iter() {
            report.pages_visited += 1;
            // Prefer externalApplicationLink when present; otherwise the
            // cryptojobs.com URL itself (applies via their on-site form).
            let apply_url = item
                .external_application_link
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&item.link)
                .to_string();
            report.apply_links_found += 1;

            let Some(ats) = classify_or_other(&apply_url) else {
                continue;
            };
            report.jobs_matched += 1;

            let (company_id, company_is_new) = match db.upsert_company(
                item.company.as_deref(),
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
            }

            let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
            let raw_json = serde_json::to_string(&item).unwrap_or_else(|_| "{}".to_string());
            match db.upsert_job(JobUpsert {
                company_id,
                kind: ats.kind,
                external_id: &external_id,
                title: &item.title,
                location: item.job_location.as_deref().filter(|s| !s.is_empty()),
                department: item.category.as_deref(),
                apply_url: &apply_url,
                description: item.description.as_deref(),
                remote: item.remote_flag(),
                posted_at: item.posted_date.as_deref(),
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

#[derive(Debug, serde::Serialize)]
struct CryptoJobsItem {
    title: String,
    link: String,
    company: Option<String>,
    category: Option<String>,
    description: Option<String>,
    job_location: Option<String>,
    work_flexibility: Option<String>,
    external_application_link: Option<String>,
    posted_date: Option<String>,
}

impl CryptoJobsItem {
    fn remote_flag(&self) -> Option<bool> {
        self.work_flexibility
            .as_deref()
            .map(|w| w.eq_ignore_ascii_case("remote"))
    }
}

fn parse_rss_items(xml: &str) -> Vec<CryptoJobsItem> {
    static ITEM_RE: OnceLock<Regex> = OnceLock::new();
    let item_re = ITEM_RE.get_or_init(|| Regex::new(r"(?s)<item>(.*?)</item>").unwrap());
    let mut out = Vec::new();
    for cap in item_re.captures_iter(xml) {
        let block = &cap[1];
        let Some(title) = extract_cdata(block, "title") else {
            continue;
        };
        let Some(link) = extract_cdata(block, "link") else {
            continue;
        };
        out.push(CryptoJobsItem {
            title,
            link,
            company: extract_cdata(block, "clientCompany"),
            category: extract_cdata(block, "category"),
            description: extract_cdata(block, "description").map(strip_html_tags),
            job_location: extract_cdata(block, "jobLocation"),
            work_flexibility: extract_cdata(block, "workFlexibility"),
            external_application_link: extract_cdata(block, "externalApplicationLink"),
            posted_date: extract_cdata(block, "postedDate"),
        });
    }
    out
}

/// Pull a `<tag><![CDATA[ ... ]]></tag>` payload, returning trimmed inner text.
/// Returns None for missing or whitespace-only fields.
fn extract_cdata(block: &str, tag: &str) -> Option<String> {
    let pat = format!(r"(?s)<{tag}>\s*(?:<!\[CDATA\[(.*?)\]\]>|([^<]*))\s*</{tag}>");
    let re = Regex::new(&pat).ok()?;
    let cap = re.captures(block)?;
    let raw = cap.get(1).or_else(|| cap.get(2)).map(|m| m.as_str())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Naive HTML-tag stripper used for the description field. Good enough for
/// FTS5 indexing; strips tags, decodes a handful of common entities, and
/// collapses whitespace.
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
    fn parse_extracts_core_fields() {
        let xml = r#"<rss><channel>
<item>
<clientCompany><![CDATA[Acme]]></clientCompany>
<title><![CDATA[Rust Engineer]]></title>
<category><![CDATA[Blockchain Development]]></category>
<workFlexibility>Remote</workFlexibility>
<jobLocation><![CDATA[]]></jobLocation>
<externalApplicationLink><![CDATA[https://boards.greenhouse.io/acme/jobs/1]]></externalApplicationLink>
<description><![CDATA[<p>Build &amp; ship.</p>]]></description>
<postedDate>Jun 02, 2026</postedDate>
<link>https://www.cryptojobs.com/job/rust-engineer-12345</link>
</item>
</channel></rss>"#;
        let items = parse_rss_items(xml);
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.title, "Rust Engineer");
        assert_eq!(it.company.as_deref(), Some("Acme"));
        assert_eq!(it.category.as_deref(), Some("Blockchain Development"));
        assert_eq!(it.remote_flag(), Some(true));
        assert_eq!(it.job_location, None); // empty CDATA → None
        assert_eq!(
            it.external_application_link.as_deref(),
            Some("https://boards.greenhouse.io/acme/jobs/1")
        );
        assert_eq!(it.description.as_deref(), Some("Build & ship."));
    }
}
