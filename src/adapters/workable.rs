use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::adapters::{
    fetch_value_or_none_on_404, html_to_text, AdapterJob, AtsAdapter, FetchedJobs,
};
use crate::ats::AtsKind;
use crate::http;

/// Workable has two unrelated company namespaces, and a company is stored
/// under whichever one it was discovered with:
///
/// - account slugs (`apply.workable.com/{slug}`), synced through the public
///   widget API, which returns the account's full job list;
/// - job-board ids (`jobs.workable.com/company/{id}`), stored with this
///   prefix (`jobs/{id}`) so the two can't collide. Workable's cross-company
///   job board exposes no account slug, so these are synced from the
///   company page instead.
pub const BOARD_SLUG_PREFIX: &str = "jobs/";

pub struct Workable;

#[async_trait(?Send)]
impl AtsAdapter for Workable {
    fn kind(&self) -> AtsKind {
        AtsKind::Workable
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        Ok(self.fetch(slug).await?.jobs)
    }

    async fn fetch(&self, slug: &str) -> Result<FetchedJobs> {
        match slug.strip_prefix(BOARD_SLUG_PREFIX) {
            Some(board_id) => fetch_board_company(board_id).await,
            None => Ok(FetchedJobs {
                jobs: fetch_account(slug).await?,
                complete: true,
            }),
        }
    }
}

// --- apply.workable.com widget API -----------------------------------------

#[derive(Deserialize, Debug)]
struct WidgetJob {
    title: String,
    shortcode: String,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    telecommuting: Option<bool>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    published_on: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

async fn fetch_account(slug: &str) -> Result<Vec<AdapterJob>> {
    let url = format!("https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true");
    let client = http::client()?;
    let Some(value) = fetch_value_or_none_on_404(&client, &url).await? else {
        anyhow::bail!("404")
    };
    let jobs_arr = value
        .get("jobs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(jobs_arr.len());
    for entry in jobs_arr {
        let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
        let j: WidgetJob = match serde_json::from_value(entry) {
            Ok(j) => j,
            Err(_) => continue,
        };
        let location = [&j.city, &j.state, &j.country]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(AdapterJob {
            apply_url: format!("https://apply.workable.com/{slug}/j/{}/", j.shortcode),
            external_id: j.shortcode,
            title: j.title,
            location: Some(location).filter(|s| !s.is_empty()),
            department: j.department,
            description: j.description.as_deref().map(html_to_text),
            remote: j.telecommuting,
            posted_at: j.published_on,
            raw_json,
        });
    }
    Ok(out)
}

// --- jobs.workable.com job board -------------------------------------------

/// One job as embedded in jobs.workable.com pages (search and company
/// pages share the shape).
#[derive(Deserialize, Serialize, Debug)]
pub struct BoardJob {
    pub id: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "requirementsSection")]
    pub requirements: Option<String>,
    #[serde(default, rename = "benefitsSection")]
    pub benefits: Option<String>,
    #[serde(default)]
    pub locations: Vec<String>,
    #[serde(default)]
    pub location: Option<BoardLocation>,
    #[serde(default)]
    pub workplace: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub company: Option<BoardCompany>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct BoardLocation {
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub subregion: Option<String>,
    #[serde(default, rename = "countryName")]
    pub country_name: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct BoardCompany {
    pub title: String,
    /// `https://jobs.workable.com/company/{board_id}/jobs-at-{name}`
    pub url: String,
    #[serde(default)]
    pub website: Option<String>,
}

impl BoardCompany {
    /// The `jobs/{id}` slug this company is stored under.
    pub fn slug(&self) -> Option<String> {
        board_company_id(&self.url).map(|id| format!("{BOARD_SLUG_PREFIX}{id}"))
    }
}

impl BoardJob {
    pub fn remote(&self) -> bool {
        self.workplace.as_deref() == Some("remote")
            || self.locations.iter().any(|l| l == "TELECOMMUTE")
    }

    pub fn location_text(&self) -> Option<String> {
        let loc = self.location.as_ref()?;
        let s = [&loc.city, &loc.subregion, &loc.country_name]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        Some(s).filter(|s| !s.is_empty())
    }

    pub fn description_text(&self) -> Option<String> {
        let html = [&self.description, &self.requirements, &self.benefits]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        Some(html_to_text(&html)).filter(|s| !s.is_empty())
    }

    pub fn into_adapter_job(self) -> AdapterJob {
        let raw_json = serde_json::to_string(&self).unwrap_or_else(|_| "{}".to_string());
        AdapterJob {
            remote: Some(self.remote()),
            location: self.location_text(),
            description: self.description_text(),
            external_id: self.id,
            title: self.title,
            department: None,
            apply_url: self.url,
            posted_at: self.created,
            raw_json,
        }
    }
}

/// Extract `{id}` from `https://jobs.workable.com/company/{id}/...`.
pub fn board_company_id(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if u.host_str()? != "jobs.workable.com" {
        return None;
    }
    let mut segs = u.path_segments()?;
    if segs.next()? != "company" {
        return None;
    }
    segs.next().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Pull the `initialState` object out of a jobs.workable.com page. The
/// page boots its client from `window.jobBoard = { …, initialState: {…},
/// … }`, keyed by the API path it would otherwise have fetched.
pub fn initial_state(html: &str) -> Result<serde_json::Value> {
    const MARKER: &str = "initialState: ";
    let start = html.find(MARKER).context("no initialState in page")? + MARKER.len();
    serde_json::Deserializer::from_str(&html[start..])
        .into_iter::<serde_json::Value>()
        .next()
        .context("empty initialState")?
        .context("parse initialState")
}

/// The `data` object of the first `initialState` entry whose key starts
/// with `key_prefix` (e.g. `api/v1/jobs`, `api/v1/companies/`).
pub fn state_data<'a>(
    state: &'a serde_json::Value,
    key_prefix: &str,
) -> Option<&'a serde_json::Value> {
    state
        .as_object()?
        .iter()
        .find(|(k, _)| k.starts_with(key_prefix))
        .and_then(|(_, v)| v.get("data"))
}

/// Parse the job array of a search or company page's `data` object,
/// skipping entries that don't match the expected shape.
pub fn parse_board_jobs(data: &serde_json::Value) -> Vec<BoardJob> {
    data.get("jobs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|j| serde_json::from_value(j.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Added to the sync loop's POLITENESS_DELAY so these server-rendered
/// pages go out at about one every two seconds. At one per second
/// jobs.workable.com answered 429 after ~340 pages.
const BOARD_EXTRA_DELAY: Duration = Duration::from_millis(1750);

/// Wait used on a 429 when the response carries no `Retry-After`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);
const MAX_RETRIES_ON_429: u32 = 2;

/// GET a jobs.workable.com page. A 429 is honoured: wait for
/// `Retry-After` (or a minute) and retry, at most `MAX_RETRIES_ON_429`
/// times, then fail. Returns `None` on 404.
pub async fn get_board_page(client: &reqwest::Client, url: &str) -> Result<Option<String>> {
    let mut retries = 0;
    loop {
        let resp = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        match resp.status().as_u16() {
            404 => return Ok(None),
            429 if retries < MAX_RETRIES_ON_429 => {
                let wait = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_RETRY_AFTER);
                tracing::warn!(
                    url,
                    wait_s = wait.as_secs(),
                    "429 from jobs.workable.com; backing off"
                );
                tokio::time::sleep(wait).await;
                retries += 1;
            }
            _ => return Ok(Some(resp.error_for_status()?.text().await?)),
        }
    }
}

async fn fetch_board_company(board_id: &str) -> Result<FetchedJobs> {
    tokio::time::sleep(BOARD_EXTRA_DELAY).await;
    let url = format!("https://jobs.workable.com/company/{board_id}");
    let client = http::client()?;
    let Some(html) = get_board_page(&client, &url).await? else {
        anyhow::bail!("404")
    };
    let state = initial_state(&html)?;
    let data = state_data(&state, "api/v1/companies/").context("no company data in page")?;
    let jobs = parse_board_jobs(data);
    // More jobs than the page carries means the rest sit behind the
    // page's pagination, which isn't followed. Keep what we have, and
    // report the list as partial so sync doesn't close the jobs it missed.
    let total = data.get("totalSize").and_then(|v| v.as_u64());
    let complete = total.map_or(true, |t| t <= jobs.len() as u64);
    if !complete {
        tracing::info!(board_id, total, got = jobs.len(), "company page truncated");
    }
    Ok(FetchedJobs {
        jobs: jobs.into_iter().map(BoardJob::into_adapter_job).collect(),
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<script>window.jobBoard = { supportedLanguages: [], initialState: {"api/v2/user":{"status":401},"api/v1/jobs":{"status":200,"data":{"totalSize":1,"jobs":[{"id":"u-1","title":"Rust Engineer","url":"https://jobs.workable.com/view/abc/rust","description":"<p>Hi &amp; welcome</p>","locations":["TELECOMMUTE","Germany"],"location":{"city":"","subregion":null,"countryName":"Germany"},"workplace":"remote","created":"2026-10-01T00:00:00Z","company":{"title":"Acme","url":"https://jobs.workable.com/company/XyZ123/jobs-at-acme","website":"https://acme.test"}}]}}}, featureFlags: {}};</script>"#;

    #[test]
    fn parses_search_page() {
        let state = initial_state(PAGE).unwrap();
        let data = state_data(&state, "api/v1/jobs").unwrap();
        let jobs = parse_board_jobs(data);
        assert_eq!(jobs.len(), 1);
        let j = &jobs[0];
        assert!(j.remote());
        assert_eq!(j.location_text().as_deref(), Some("Germany"));
        assert_eq!(j.description_text().as_deref(), Some("Hi & welcome"));
        assert_eq!(
            j.company.as_ref().unwrap().slug().as_deref(),
            Some("jobs/XyZ123")
        );
    }

    #[test]
    fn board_company_id_only_on_company_pages() {
        assert_eq!(
            board_company_id("https://jobs.workable.com/company/XyZ123/jobs-at-acme").as_deref(),
            Some("XyZ123")
        );
        assert_eq!(
            board_company_id("https://jobs.workable.com/view/abc/rust"),
            None
        );
        assert_eq!(
            board_company_id("https://apply.workable.com/company/x"),
            None
        );
    }
}
