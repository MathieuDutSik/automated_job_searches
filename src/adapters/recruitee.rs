use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_value_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct Recruitee;

#[derive(Deserialize, Debug)]
struct RecruiteeJob {
    id: serde_json::Value,
    title: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default, rename = "careers_url")]
    careers_url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    requirements: Option<String>,
    #[serde(default)]
    remote: Option<bool>,
    #[serde(default, rename = "published_at")]
    published_at: Option<String>,
}

#[async_trait(?Send)]
impl AtsAdapter for Recruitee {
    fn kind(&self) -> AtsKind {
        AtsKind::Recruitee
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://{slug}.recruitee.com/api/offers/");
        let client = http::client()?;
        let Some(value) = fetch_value_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        let offers = value
            .get("offers")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(offers.len());
        for entry in offers {
            let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
            let j: RecruiteeJob = match serde_json::from_value(entry) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let external_id = j.id.to_string().trim_matches('"').to_string();
            let apply_url = j
                .careers_url
                .clone()
                .unwrap_or_else(|| format!("https://{slug}.recruitee.com/o/{external_id}"));
            // Recruitee splits the job text into `description` (the role
            // intro) and `requirements` (the must-have list). Concatenate
            // both for FTS so keyword search hits either side.
            let combined = match (&j.description, &j.requirements) {
                (Some(d), Some(r)) => Some(format!("{d}\n\n{r}")),
                (Some(d), None) => Some(d.clone()),
                (None, Some(r)) => Some(r.clone()),
                (None, None) => None,
            };
            let description = combined.map(|s| crate::adapters::html_to_text(&s));
            out.push(AdapterJob {
                external_id,
                title: j.title,
                location: j.location,
                department: j.department,
                apply_url,
                description,
                remote: j.remote,
                posted_at: j.published_at,
                raw_json,
            });
        }
        Ok(out)
    }
}
