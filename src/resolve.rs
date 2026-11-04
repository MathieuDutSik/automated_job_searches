//! `ajs resolve`: find the ATS behind `other` companies.
//!
//! Crawlers store a company as `other` when its link is a careers page or
//! homepage rather than a known ATS board, and `sync` can't do anything
//! with those. Many careers pages embed or link a Greenhouse / Ashby /
//! Lever / ... board, so for each `other` company this visits a few of
//! its own pages (the stored link, a careers link found there, then
//! `/careers` and `/jobs`), classifies every absolute URL it sees, and
//! when one names a known ATS upserts that company so `sync` picks it up.
//!
//! Every request checks the host's robots.txt first and the run waits
//! between requests. Each company's attempt is recorded in
//! `resolve_attempts`, so a re-run only tries new companies.

use anyhow::Result;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;
use texting_robots::Robot;
use tracing::{debug, info, warn};
use url::Url;

use crate::ats::{classify_apply_url, AtsKind, AtsRef};
use crate::db::Db;
use crate::http;

const SOURCE: &str = "resolve";
/// The product token sent by src/http.rs, for robots.txt matching.
const ROBOTS_AGENT: &str = "ajs";
const MAX_PAGES_PER_COMPANY: usize = 4;
const REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// Hosts that never front a single company's careers page.
const SKIP_HOSTS: &[&str] = &[
    "drive.google.com",
    "docs.google.com",
    "forms.gle",
    "www.linkedin.com",
    "linkedin.com",
    "www.youtube.com",
    "youtube.com",
    "x.com",
    "twitter.com",
    "github.com",
    "news.ycombinator.com",
];

#[derive(Debug, Default)]
pub struct ResolveReport {
    pub companies_tried: u64,
    pub resolved: u64,
    pub companies_new: u64,
    pub pages_fetched: u64,
    pub robots_blocked: u64,
}

/// A company the resolve pass inserted, to be synced right away.
pub struct Fresh {
    pub kind: AtsKind,
    pub company_id: i64,
    pub slug: String,
}

pub async fn run(db: &Db, include_attempted: bool) -> Result<(ResolveReport, Vec<Fresh>)> {
    let client = http::client()?;
    let mut robots = RobotsCache::default();
    let mut report = ResolveReport::default();
    let mut fresh = Vec::new();
    let others = db.list_unresolved_others(include_attempted)?;
    info!(count = others.len(), "other companies to resolve");

    for (company_id, name, host_slug, urls) in others {
        let Some(start) = start_url(&host_slug, &urls) else {
            debug!(host_slug, "no usable URL");
            db.record_resolve_attempt(company_id, None)?;
            continue;
        };
        report.companies_tried += 1;
        let found = resolve_company(&client, &mut robots, &start, &mut report).await;
        let result = match found {
            Some((ats, page)) => {
                let slug_label = format!("{}/{}", ats.kind.as_str(), ats.slug);
                info!(company = %name, from = %host_slug, to = %slug_label, page = %page, "resolved");
                report.resolved += 1;
                match db.upsert_company(Some(&name), ats.kind, &ats.slug, SOURCE, Some(&page)) {
                    Ok((id, is_new)) => {
                        if is_new {
                            report.companies_new += 1;
                            fresh.push(Fresh {
                                kind: ats.kind,
                                company_id: id,
                                slug: ats.slug.clone(),
                            });
                        }
                    }
                    Err(e) => warn!(error = %e, slug = %ats.slug, "company upsert failed"),
                }
                Some(slug_label)
            }
            None => None,
        };
        db.record_resolve_attempt(company_id, result.as_deref())?;
    }
    Ok((report, fresh))
}

/// The first URL to visit: a stored link that looks like a careers page,
/// else any stored link, else the host root. Links on the company's own
/// host come first, since a comment can also link another company.
fn start_url(host_slug: &str, urls: &[String]) -> Option<Url> {
    let own = |u: &Url| {
        u.host_str()
            .is_some_and(|h| h.trim_start_matches("www.") == host_slug.trim_start_matches("www."))
    };
    let mut parsed: Vec<Url> = urls
        .iter()
        .filter_map(|u| Url::parse(u).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .collect();
    if parsed.iter().any(own) {
        parsed.retain(own);
    }
    let pick = parsed
        .iter()
        .find(|u| looks_like_careers_path(u.path()))
        .or_else(|| parsed.first())
        .cloned()
        .or_else(|| Url::parse(&format!("https://{host_slug}/")).ok())?;
    let host = pick.host_str()?;
    if SKIP_HOSTS.contains(&host) {
        return None;
    }
    Some(pick)
}

async fn resolve_company(
    client: &reqwest::Client,
    robots: &mut RobotsCache,
    start: &Url,
    report: &mut ResolveReport,
) -> Option<(AtsRef, String)> {
    let mut queue: Vec<Url> = vec![start.clone()];
    for path in ["/careers", "/jobs"] {
        if let Ok(u) = start.join(path) {
            queue.push(u);
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut idx = 0;
    while idx < queue.len() && seen.len() < MAX_PAGES_PER_COMPANY {
        let url = queue[idx].clone();
        idx += 1;
        if !seen.insert(url.as_str().trim_end_matches('/').to_string()) {
            continue;
        }
        if !robots.allowed(client, &url).await {
            debug!(url = %url, "robots.txt disallows");
            report.robots_blocked += 1;
            continue;
        }
        tokio::time::sleep(REQUEST_DELAY).await;
        let resp = match client.get(url.as_str()).send().await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, url = %url, "fetch failed");
                continue;
            }
        };
        report.pages_fetched += 1;
        // A redirect straight to an ATS board (grnh.se, custom careers
        // domains) is the clearest answer.
        let final_url = resp.url().clone();
        if let Some(ats) = known_ats(final_url.as_str()) {
            return Some((ats, final_url.to_string()));
        }
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.text().await else {
            continue;
        };
        if let Some(ats) = best_ats_in_page(&body) {
            return Some((ats, final_url.to_string()));
        }
        // Only the first page contributes a careers link, so the queue
        // stays at the start page, one discovered link, /careers, /jobs.
        if idx == 1 {
            if let Some(link) = careers_link(&body, &final_url) {
                queue.insert(1, link);
            }
        }
    }
    None
}

fn known_ats(url: &str) -> Option<AtsRef> {
    classify_apply_url(url).filter(|r| r.kind != AtsKind::Other)
}

/// Classify every absolute URL in the page and return the ATS board
/// named most often, so a lone link to some other company's board
/// doesn't win over the page's own embed.
fn best_ats_in_page(body: &str) -> Option<AtsRef> {
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let re = URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s"'<>`)\\]+"#).unwrap());
    // JSON-embedded links are often written `https:\/\/…`.
    let body = body.replace("\\/", "/").replace("&amp;", "&");
    let mut counts: HashMap<(AtsKind, String), (usize, AtsRef)> = HashMap::new();
    for m in re.find_iter(&body) {
        if let Some(ats) = known_ats(m.as_str()) {
            let entry = counts
                .entry((ats.kind, ats.slug.clone()))
                .or_insert((0, ats));
            entry.0 += 1;
        }
    }
    counts
        .into_values()
        .max_by_key(|(n, _)| *n)
        .map(|(_, ats)| ats)
}

fn looks_like_careers_path(path: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(career|jobs|join-us|joinus|hiring|work-with-us|open-positions|vacanc)")
            .unwrap()
    })
    .is_match(path)
}

/// First same-site link on the page whose path looks like a careers page.
fn careers_link(body: &str, base: &Url) -> Option<Url> {
    static HREF_RE: OnceLock<Regex> = OnceLock::new();
    let re = HREF_RE.get_or_init(|| Regex::new(r#"href\s*=\s*["']([^"'#]+)["']"#).unwrap());
    let base_host = base.host_str()?.trim_start_matches("www.");
    re.captures_iter(body)
        .filter_map(|c| base.join(c[1].trim()).ok())
        .find(|u| {
            u.host_str()
                .is_some_and(|h| h.trim_start_matches("www.") == base_host)
                && looks_like_careers_path(u.path())
        })
}

/// robots.txt per host, fetched once. A 4xx means no rules (allowed); a
/// 5xx or network error means we can't tell, so the host is skipped.
#[derive(Default)]
struct RobotsCache {
    by_host: HashMap<String, Option<Robot>>,
}

enum RobotsState {
    Rules(Robot),
    AllowAll,
    Unknown,
}

impl RobotsCache {
    async fn allowed(&mut self, client: &reqwest::Client, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let key = format!("{}://{host}", url.scheme());
        if !self.by_host.contains_key(&key) {
            let state = fetch_robots(client, &key).await;
            let entry = match state {
                RobotsState::Rules(r) => Some(r),
                RobotsState::AllowAll => None,
                RobotsState::Unknown => {
                    // Store a robot that disallows everything.
                    Robot::new(ROBOTS_AGENT, b"User-agent: *\nDisallow: /\n").ok()
                }
            };
            self.by_host.insert(key.clone(), entry);
        }
        match self.by_host.get(&key) {
            Some(Some(robot)) => robot.allowed(url.as_str()),
            _ => true,
        }
    }
}

async fn fetch_robots(client: &reqwest::Client, origin: &str) -> RobotsState {
    let url = format!("{origin}/robots.txt");
    let Ok(resp) = client.get(&url).send().await else {
        return RobotsState::Unknown;
    };
    let status = resp.status();
    if status.is_client_error() {
        return RobotsState::AllowAll;
    }
    if !status.is_success() {
        return RobotsState::Unknown;
    }
    match resp.bytes().await {
        Ok(body) => match Robot::new(ROBOTS_AGENT, &body) {
            Ok(r) => RobotsState::Rules(r),
            Err(_) => RobotsState::AllowAll,
        },
        Err(_) => RobotsState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_most_frequent_board() {
        let page = r#"<iframe src="https://boards.greenhouse.io/embed/job_board?for=acme"></iframe>
            <a href="https://boards.greenhouse.io/acme/jobs/1">a</a>
            <a href="https://jobs.lever.co/othercorp">partner</a>"#;
        let r = best_ats_in_page(page).unwrap();
        assert_eq!(r.kind, AtsKind::Greenhouse);
        assert_eq!(r.slug, "acme");
    }

    #[test]
    fn reads_json_escaped_links() {
        let page = r#"{"jobsUrl":"https:\/\/jobs.ashbyhq.com\/acme"}"#;
        assert_eq!(best_ats_in_page(page).unwrap().slug, "acme");
    }

    #[test]
    fn finds_same_site_careers_link() {
        let base = Url::parse("https://www.acme.test/").unwrap();
        let page = r#"<a href="https://blog.other.test/careers">x</a><a href="/about">a</a><a href="/company/careers">c</a>"#;
        assert_eq!(
            careers_link(page, &base).unwrap().as_str(),
            "https://www.acme.test/company/careers"
        );
    }

    #[test]
    fn start_url_prefers_careers_and_skips_generic_hosts() {
        let urls = vec![
            "https://acme.test/".to_string(),
            "https://acme.test/careers/rust".to_string(),
        ];
        assert_eq!(
            start_url("acme.test", &urls).unwrap().as_str(),
            "https://acme.test/careers/rust"
        );
        assert!(start_url(
            "drive.google.com",
            &["https://drive.google.com/file/x".into()]
        )
        .is_none());
        assert_eq!(
            start_url("acme.test", &[]).unwrap().as_str(),
            "https://acme.test/"
        );
        let urls = vec![
            "https://other.test/about/careers".to_string(),
            "https://www.acme.test/about".to_string(),
        ];
        assert_eq!(
            start_url("www.acme.test", &urls).unwrap().as_str(),
            "https://www.acme.test/about"
        );
    }
}
