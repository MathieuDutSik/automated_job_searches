use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;

use crate::adapters::{fetch_value_or_none_on_404, AdapterJob, AtsAdapter};
use crate::ats::AtsKind;
use crate::http;

pub struct BambooHr;

#[derive(Deserialize, Debug)]
struct BambooLocation {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize, Debug)]
struct BambooJob {
    id: serde_json::Value,
    #[serde(rename = "jobOpeningName")]
    job_opening_name: String,
    #[serde(default, rename = "departmentLabel")]
    department_label: Option<String>,
    #[serde(default, rename = "employmentStatusLabel")]
    employment_status_label: Option<String>,
    #[serde(default)]
    location: Option<BambooLocation>,
    #[serde(default, rename = "isRemote")]
    is_remote: Option<bool>,
    #[serde(default, rename = "locationType")]
    location_type: Option<String>,
}

fn format_location(l: &Option<BambooLocation>) -> Option<String> {
    let loc = l.as_ref()?;
    match (loc.city.as_deref(), loc.state.as_deref()) {
        (Some(c), Some(s)) if !c.is_empty() && !s.is_empty() => Some(format!("{c}, {s}")),
        (Some(c), _) if !c.is_empty() => Some(c.to_string()),
        (_, Some(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

#[async_trait(?Send)]
impl AtsAdapter for BambooHr {
    fn kind(&self) -> AtsKind {
        AtsKind::Bamboohr
    }

    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>> {
        let url = format!("https://{slug}.bamboohr.com/careers/list");
        let client = http::client()?;
        let Some(value) = fetch_value_or_none_on_404(&client, &url).await? else {
            anyhow::bail!("404")
        };
        let arr = value
            .get("result")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(arr.len());
        for entry in arr {
            let raw_json = serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string());
            let j: BambooJob = match serde_json::from_value(entry) {
                Ok(j) => j,
                Err(_) => continue,
            };
            let external_id = j.id.to_string().trim_matches('"').to_string();
            let apply_url = format!("https://{slug}.bamboohr.com/careers/{external_id}");
            // locationType "2" or location values containing "remote" reinforce
            // the structured isRemote flag when BambooHR leaves it null.
            let remote = j.is_remote.or_else(|| {
                let loc_remote = format_location(&j.location)
                    .map(|s| s.to_ascii_lowercase().contains("remote"));
                let type_remote = j.location_type.as_deref().map(|t| t == "2");
                loc_remote.or(type_remote)
            });
            out.push(AdapterJob {
                external_id,
                title: j.job_opening_name,
                location: format_location(&j.location),
                department: j.department_label,
                apply_url,
                description: None,
                remote,
                posted_at: None,
                raw_json,
            });
            let _ = j.employment_status_label; // captured in raw_json for now
        }
        Ok(out)
    }
}
