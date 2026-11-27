use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

use crate::adapters::html_to_text;
use crate::ats::{classify_apply_url, classify_or_other};
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const SITEMAP_URL: &str = "https://remotifyeurope.com/sitemap.xml";
const SOURCE: &str = "remotifyeurope";
/// robots.txt sets no Crawl-delay; one page a second is our own restraint.
const POLITENESS_DELAY: Duration = Duration::from_millis(1000);

/// RemotifyEurope (remotifyeurope.com), a board of remote jobs open to
/// Europe. robots.txt allows `/listing/` and the sitemap lists every
/// live listing; each listing page embeds a schema.org `JobPosting`
/// (title, employer, dates, `TELECOMMUTE`, and the countries applicants
/// must be in) and links the employer's own ATS posting, which is what
/// files the job under the right company.
///
/// Cloudflare watches the site in the background. A 403 or 429 means it
/// has started refusing us, so the crawl stops there instead of pressing on.
pub struct RemotifyEurope;

#[async_trait(?Send)]
impl Crawler for RemotifyEurope {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = SITEMAP_URL, "fetching sitemap");
        let resp = client.get(SITEMAP_URL).send().await.context("GET remotifyeurope sitemap")?;
        let status = resp.status();
        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }
        let pages = listing_pages(&resp.text().await?);
        info!(count = pages.len(), "listing pages in sitemap");
        let now = chrono::Utc::now().to_rfc3339();

        for (idx, page) in pages.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            let resp = match client.get(page).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, url = %page, "listing fetch failed");
                    continue;
                }
            };
            let code = resp.status().as_u16();
            if code == 403 || code == 429 {
                anyhow::bail!("{code} from remotifyeurope.com at {page}; stopping (refused)");
            }
            if !resp.status().is_success() {
                continue;
            }
            report.pages_visited += 1;
            let Ok(html) = resp.text().await else { continue };
            let Some(listing) = parse_listing(&html) else { continue };
            if listing.valid_through.as_deref().is_some_and(|v| v < now.as_str()) {
                continue;
            }
            store(db, &listing, page, &mut report);
            if (idx + 1) % 100 == 0 {
                info!(done = idx + 1, of = pages.len(), jobs_new = report.jobs_new, "progress");
            }
        }
        Ok(report)
    }
}

#[derive(Debug, Default, serde::Serialize)]
struct Listing {
    title: String,
    company: Option<String>,
    date_posted: Option<String>,
    valid_through: Option<String>,
    remote: bool,
    /// `applicantLocationRequirements` names, e.g. ["Germany", "Poland"].
    locations: Vec<String>,
    description: Option<String>,
    /// The employer's own posting (an ATS URL) when the page links one.
    apply_url: Option<String>,
    /// `hiringOrganization.sameAs`, the employer's website.
    company_url: Option<String>,
}

fn listing_pages(sitemap: &str) -> Vec<String> {
    static LOC: OnceLock<Regex> = OnceLock::new();
    let re = LOC.get_or_init(|| {
        Regex::new(r"<loc>\s*(https://remotifyeurope\.com/listing/[^<\s]+)\s*</loc>").unwrap()
    });
    re.captures_iter(sitemap).map(|c| c[1].to_string()).collect()
}

fn parse_listing(html: &str) -> Option<Listing> {
    static LD: OnceLock<Regex> = OnceLock::new();
    let ld = LD.get_or_init(|| {
        Regex::new(r#"(?s)<script type="application/ld\+json">(.*?)</script>"#).unwrap()
    });
    let posting = ld.captures_iter(html).find_map(|c| {
        let v: serde_json::Value = serde_json::from_str(&c[1]).ok()?;
        (v.get("@type")?.as_str()? == "JobPosting").then_some(v)
    })?;
    let s = |k: &str| posting.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let locations = match posting.get("applicantLocationRequirements") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect(),
        Some(o) => o.get("name").and_then(|n| n.as_str()).map(str::to_string).into_iter().collect(),
        None => Vec::new(),
    };
    Some(Listing {
        title: s("title")?,
        company: posting
            .pointer("/hiringOrganization/name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        date_posted: s("datePosted"),
        valid_through: s("validThrough"),
        remote: s("jobLocationType").as_deref() == Some("TELECOMMUTE"),
        locations,
        description: s("description").map(|d| html_to_text(&d)).filter(|d| !d.is_empty()),
        apply_url: ats_link(html),
        company_url: posting
            .pointer("/hiringOrganization/sameAs")
            .and_then(|v| v.as_str())
            .map(strip_utm),
    })
}

/// First link on the page to a known ATS posting, without tracking params.
fn ats_link(html: &str) -> Option<String> {
    static HREF: OnceLock<Regex> = OnceLock::new();
    let re = HREF.get_or_init(|| Regex::new(r#"https?://[^"'\s<>\\]+"#).unwrap());
    re.find_iter(html)
        .map(|m| m.as_str().replace("&amp;", "&"))
        .find(|u| classify_apply_url(u).is_some())
        .map(|u| strip_utm(&u))
}

fn strip_utm(u: &str) -> String {
    let Ok(mut url) = url::Url::parse(u) else { return u.to_string() };
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    url.to_string()
}

fn store(db: &Db, l: &Listing, page: &str, report: &mut CrawlReport) {
    // Without an ATS link the employer applies through its own site: file
    // the job under the employer's website (`resolve` may later find its
    // board) and keep the listing as the job's URL. Classifying the
    // listing URL itself would put every such employer under one
    // "remotifyeurope.com" company.
    let (ats, apply_url) = match &l.apply_url {
        Some(u) => match classify_or_other(u) {
            Some(a) => (a, u.clone()),
            None => return,
        },
        None => {
            let Some(site) = l.company_url.as_deref().and_then(classify_or_other) else {
                return;
            };
            let mut a = site;
            a.external_id = Some(page.to_string());
            (a, page.to_string())
        }
    };
    report.apply_links_found += 1;
    report.jobs_matched += 1;
    let (company_id, is_new) =
        match db.upsert_company(l.company.as_deref(), ats.kind, &ats.slug, SOURCE, Some(page)) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, slug = %ats.slug, "company upsert failed");
                return;
            }
        };
    if is_new {
        report.companies_new += 1;
    }
    let location = l.locations.join(", ");
    let external_id = ats.external_id.clone().unwrap_or_else(|| apply_url.clone());
    let raw_json = serde_json::to_string(l).unwrap_or_else(|_| "{}".to_string());
    match db.upsert_job(JobUpsert {
        source: SOURCE,
        company_id,
        kind: ats.kind,
        external_id: &external_id,
        title: l.title.trim(),
        location: Some(location.as_str()).filter(|s| !s.is_empty()),
        department: None,
        apply_url: &apply_url,
        description: l.description.as_deref(),
        remote: Some(l.remote),
        posted_at: l.date_posted.as_deref(),
        raw_json: &raw_json,
    }) {
        Ok((_, is_new)) => {
            if is_new {
                report.jobs_new += 1;
            }
        }
        Err(e) => warn!(error = %e, title = %l.title, "job upsert failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<script type="application/ld+json">{"@context":"https://schema.org","@type":"JobPosting","title":"Rust Engineer","hiringOrganization":{"@type":"Organization","name":"Acme","sameAs":"https://acme.test/?utm_source=RemotifyEurope"},"datePosted":"2026-11-27T08:02:22.800Z","validThrough":"2026-12-27T00:00:00.000Z","jobLocationType":"TELECOMMUTE","applicantLocationRequirements":[{"@type":"Country","name":"Germany"},{"@type":"Country","name":"Poland"}],"description":"<p>Build &amp; ship</p>"}</script>
        <a href="https://remotifyeurope.com/pricing">x</a>
        <a href="https://job-boards.greenhouse.io/acme/jobs/8419601?utm_source=RemotifyEurope">Apply</a>"#;

    #[test]
    fn parses_listing() {
        let l = parse_listing(PAGE).unwrap();
        assert_eq!(l.title, "Rust Engineer");
        assert_eq!(l.company.as_deref(), Some("Acme"));
        assert!(l.remote);
        assert_eq!(l.locations, vec!["Germany", "Poland"]);
        assert_eq!(l.description.as_deref(), Some("Build & ship"));
        assert_eq!(
            l.apply_url.as_deref(),
            Some("https://job-boards.greenhouse.io/acme/jobs/8419601")
        );
        assert_eq!(l.company_url.as_deref(), Some("https://acme.test/"));
    }

    #[test]
    fn sitemap_listings_only() {
        let xml = "<urlset><url><loc>https://remotifyeurope.com/listing/abc</loc></url>\
                   <url><loc>https://remotifyeurope.com/blog/x</loc></url></urlset>";
        assert_eq!(listing_pages(xml), vec!["https://remotifyeurope.com/listing/abc"]);
    }
}
