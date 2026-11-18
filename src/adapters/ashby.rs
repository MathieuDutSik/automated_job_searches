use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_value_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Ashby;

#[derive(Deserialize, Debug)]
struct AshbyJob {
    id: String,
    title: String,
    /// Older API field; current responses use `location`.
    #[serde(default, rename = "locationName")]
    location_name: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "secondaryLocations")]
    secondary_locations: Option<Vec<SecondaryLocation>>,
    /// "Remote" | "Hybrid" | "OnSite"; more precise than `isRemote`, which
    /// is also true for hybrid roles.
    #[serde(default, rename = "workplaceType")]
    workplace_type: Option<String>,
    #[serde(default, rename = "departmentName")]
    department_name: Option<String>,
    #[serde(default, rename = "jobUrl")]
    job_url: Option<String>,
    #[serde(default, rename = "publishedAt")]
    published_at: Option<String>,
    #[serde(default, rename = "isRemote")]
    is_remote: Option<bool>,
    #[serde(default, rename = "descriptionPlain")]
    description_plain: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SecondaryLocation {
    #[serde(default)]
    location: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for Ashby {
    fn kind(&self) -> AtsKind {
        AtsKind::Ashbyhq
    }

    /// The API has no organisation name; the public board page's title is
    /// "{Name} Jobs".
    async fn company_name(&self, slug: &str) -> Result<Option<String>> {
        let client = http::client()?;
        let title = crate::adapters::page_title(&client, &format!("https://jobs.ashbyhq.com/{slug}")).await?;
        Ok(title.map(|t| t.trim_end_matches(" Jobs").trim().to_string()))
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!(
            "https://api.ashbyhq.com/posting-api/job-board/{slug}?includeCompensation=true"
        );
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
            let j: AshbyJob = match serde_json::from_value(entry) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let apply_url = j
                .job_url
                .clone()
                .unwrap_or_else(|| format!("https://jobs.ashbyhq.com/{slug}/{}", j.id));
            let location = j
                .location
                .or(j.location_name)
                .into_iter()
                .chain(j.secondary_locations.into_iter().flatten().filter_map(|l| l.location))
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            let remote = match j.workplace_type.as_deref() {
                Some("Remote") => Some(true),
                Some("Hybrid" | "OnSite") => Some(false),
                _ => j.is_remote,
            };
            out.push(AdapterJob {
                external_id: j.id,
                title: j.title,
                location: Some(location).filter(|l| !l.is_empty()),
                department: j.department_name,
                apply_url,
                description: j.description_plain,
                remote,
                posted_at: j.published_at,
                raw_json,
            });
        }
        Ok(out)
    }
}
