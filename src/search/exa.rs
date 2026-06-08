use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use std::sync::OnceLock;

use crate::http;
use crate::search::{SearchEngine, SearchHit};

const ENDPOINT: &str = "https://api.exa.ai/search";
const NAME: &str = "exa";

/// EXA.AI — neural/keyword search. Their `site:` operator support is poor;
/// the canonical way to filter by host is the `includeDomains` body field.
/// This adapter extracts any `site:foo.com` token from the query and moves
/// it into `includeDomains` so existing PLANS work unchanged.
pub struct Exa {
    api_key: String,
}

impl Exa {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("EXA_SECRET_KEY").context("EXA_SECRET_KEY env var not set")?;
        if key.trim().is_empty() {
            anyhow::bail!("EXA_SECRET_KEY is empty");
        }
        Ok(Self::new(key))
    }
}

#[derive(Deserialize, Debug)]
struct ExaResponse {
    #[serde(default)]
    results: Vec<ExaHit>,
}

#[derive(Deserialize, Debug)]
struct ExaHit {
    url: String,
}

/// Pull out the domain from any `site:foo.com` token in the query. Also
/// honors a leading `-` for `-site:` (excludes); only positive `site:` is
/// captured. Returns the matched domains (positive includes only).
fn extract_site_includes(query: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?:^|\s)site:([^\s]+)").unwrap());
    re.captures_iter(query).map(|c| c[1].to_string()).collect()
}

/// Remove every `site:foo.com` and `-site:foo.com` token from the query
/// (leading minus tolerated) so EXA gets a clean keyword string.
fn strip_site_tokens(query: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"-?site:[^\s]+").unwrap());
    let cleaned = re.replace_all(query, "");
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[async_trait(?Send)]
impl SearchEngine for Exa {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn search(&self, query: &str, count: u32) -> Result<Vec<SearchHit>> {
        let client = http::client()?;
        let includes = extract_site_includes(query);
        let cleaned = strip_site_tokens(query);
        // EXA rejects empty queries; default to a generic prompt when the
        // input was *just* a site: filter (the includeDomains list will do
        // the actual narrowing).
        let q_for_body = if cleaned.is_empty() {
            "remote jobs".to_string()
        } else {
            cleaned
        };
        let mut body = json!({
            "query": q_for_body,
            "numResults": count,
            "type": "keyword",
        });
        if !includes.is_empty() {
            body["includeDomains"] = json!(includes);
        }
        let resp = client
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("EXA POST {ENDPOINT}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("EXA returned {}: {}", status, body);
        }
        let parsed: ExaResponse = resp.json().await.context("parse EXA JSON")?;
        Ok(parsed
            .results
            .into_iter()
            .map(|h| SearchHit { url: h.url })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_site_include() {
        assert_eq!(
            extract_site_includes(r#"site:jobs.ashbyhq.com "Remote""#),
            vec!["jobs.ashbyhq.com".to_string()]
        );
    }

    #[test]
    fn extracts_multiple_site_includes() {
        let v = extract_site_includes("foo site:a.com bar site:b.com baz");
        assert_eq!(v, vec!["a.com".to_string(), "b.com".to_string()]);
    }

    #[test]
    fn strip_leaves_keywords() {
        assert_eq!(
            strip_site_tokens(r#"site:bamboohr.com engineer remote"#),
            "engineer remote".to_string()
        );
    }

    #[test]
    fn strip_removes_negative_site() {
        assert_eq!(
            strip_site_tokens(r#"site:bamboohr.com -site:www.bamboohr.com remote"#),
            "remote".to_string()
        );
    }

    #[test]
    fn negative_site_is_not_extracted_as_include() {
        // `-site:` is dropped from the query but never added to includeDomains.
        let v = extract_site_includes("-site:www.foo.com remote");
        assert!(v.is_empty(), "unexpected includes: {v:?}");
    }
}
