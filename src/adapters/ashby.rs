use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Ashby;

#[derive(Deserialize, Debug)]
struct AshbyResp {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Deserialize, Debug)]
struct AshbyJob {
    id: String,
    title: String,
    #[serde(default, rename = "locationName")]
    location_name: Option<String>,
    #[serde(default, rename = "departmentName")]
    department_name: Option<String>,
    #[serde(default, rename = "jobUrl")]
    job_url: Option<String>,
    #[serde(default, rename = "publishedDate")]
    published_date: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for Ashby {
    fn kind(&self) -> AtsKind {
        AtsKind::Ashbyhq
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://api.ashbyhq.com/posting-api/job-board/{slug}");
        let client = http::client()?;
        let Some(resp): Option<AshbyResp> = fetch_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        Ok(resp
            .jobs
            .into_iter()
            .filter_map(|j| {
                let apply_url = j.job_url.clone().unwrap_or_else(|| {
                    format!("https://jobs.ashbyhq.com/{slug}/{}", j.id)
                });
                Some(AdapterJob {
                    external_id: j.id,
                    title: j.title,
                    location: j.location_name,
                    department: j.department_name,
                    apply_url,
                    posted_at: j.published_date,
                    raw_json: "{}".to_string(),
                })
            })
            .collect())
    }
}
