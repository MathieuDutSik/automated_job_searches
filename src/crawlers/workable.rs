use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use std::time::Duration;
use tracing::{info, warn};

use crate::adapters::workable::{
    get_board_page, initial_state, parse_board_jobs, state_data, BoardJob,
};
use crate::ats::AtsKind;
use crate::crawlers::{CrawlReport, Crawler};
use crate::db::{Db, JobUpsert};
use crate::http;

const SITEMAP_URL: &str = "https://jobs.workable.com/sitemap.xml";
const SOURCE: &str = "workable";
/// robots.txt sets no Crawl-delay; one HTML page per second is our own
/// restraint for a site that renders every search server-side.
const POLITENESS_DELAY: Duration = Duration::from_millis(1000);

/// Workable's cross-company job board, jobs.workable.com. Its sitemap
/// lists ~2k `/search/{country}/{query}` landing pages; robots.txt allows
/// `/search/*` but disallows query strings, so each page is read once,
/// without following its pagination. Only the `remote-*` searches are
/// visited. Every page embeds ~20 full job records, which are stored
/// directly; their companies land under `jobs/{id}` slugs that
/// `ajs sync workable` then refreshes from the company pages.
pub struct WorkableBoard;

#[async_trait(?Send)]
impl Crawler for WorkableBoard {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn run(&self, db: &Db) -> Result<CrawlReport> {
        let client = http::client()?;
        info!(url = SITEMAP_URL, "fetching sitemap");
        let resp = client
            .get(SITEMAP_URL)
            .send()
            .await
            .context("GET workable sitemap")?;
        let status = resp.status();
        let mut report = CrawlReport {
            source: SOURCE,
            http_status: Some(status.as_u16()),
            ..Default::default()
        };
        if !status.is_success() {
            return Ok(report);
        }
        let sitemap = resp.text().await?;
        let pages = remote_search_pages(&sitemap);
        info!(count = pages.len(), "remote search pages in sitemap");

        for (idx, page) in pages.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(POLITENESS_DELAY).await;
            }
            let jobs = match fetch_search_page(&client, page).await {
                Ok(j) => j,
                Err(e) => {
                    warn!(error = %e, url = %page, "search page failed");
                    continue;
                }
            };
            report.pages_visited += 1;
            for job in jobs {
                report.apply_links_found += 1;
                store_job(db, job, &mut report);
            }
            if (idx + 1) % 50 == 0 {
                info!(
                    done = idx + 1,
                    of = pages.len(),
                    jobs_new = report.jobs_new,
                    companies_new = report.companies_new,
                    "progress"
                );
            }
        }
        Ok(report)
    }
}

fn remote_search_pages(sitemap: &str) -> Vec<String> {
    let loc = Regex::new(r"<loc>\s*([^<\s]+)\s*</loc>").unwrap();
    loc.captures_iter(sitemap)
        .map(|c| c[1].to_string())
        .filter(|u| u.contains("/search/") && u.contains("/remote-") && !u.contains('?'))
        .collect()
}

async fn fetch_search_page(client: &reqwest::Client, url: &str) -> Result<Vec<BoardJob>> {
    let html = get_board_page(client, url).await?.context("404")?;
    let state = initial_state(&html)?;
    let data = state_data(&state, "api/v1/jobs").context("no job data in page")?;
    Ok(parse_board_jobs(data))
}

fn store_job(db: &Db, job: BoardJob, report: &mut CrawlReport) {
    let Some(company) = job.company.as_ref() else {
        return;
    };
    let Some(slug) = company.slug() else {
        return;
    };
    report.jobs_matched += 1;
    let company_id = match db.upsert_company(
        Some(&company.title),
        AtsKind::Workable,
        &slug,
        SOURCE,
        Some(&company.url),
    ) {
        Ok((id, is_new)) => {
            if is_new {
                report.companies_new += 1;
            }
            id
        }
        Err(e) => {
            warn!(error = %e, slug, "company upsert failed");
            return;
        }
    };
    let j = job.into_adapter_job();
    match db.upsert_job(JobUpsert {
        company_id,
        kind: AtsKind::Workable,
        external_id: &j.external_id,
        title: j.title.trim(),
        location: j.location.as_deref(),
        department: None,
        apply_url: &j.apply_url,
        description: j.description.as_deref(),
        remote: j.remote,
        posted_at: j.posted_at.as_deref(),
        raw_json: &j.raw_json,
    }) {
        Ok((_, is_new)) => {
            if is_new {
                report.jobs_new += 1;
            }
        }
        Err(e) => warn!(error = %e, title = %j.title, "job upsert failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_remote_search_pages() {
        let xml = "<urlset>\
            <url><loc>https://jobs.workable.com/search/germany/remote-software-engineer-jobs</loc></url>\
            <url><loc>https://jobs.workable.com/search/germany/hybrid-marketing-jobs</loc></url>\
            <url><loc>https://jobs.workable.com/search/germany/remote-jobs?page=2</loc></url>\
            </urlset>";
        assert_eq!(
            remote_search_pages(xml),
            vec!["https://jobs.workable.com/search/germany/remote-software-engineer-jobs"]
        );
    }
}
