use anyhow::Result;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::ats::{classify_apply_url, AtsKind};
use crate::db::Db;
use crate::search::SearchEngine;

/// How many results to ask the engine for per query. Brave caps at 20.
const RESULTS_PER_QUERY: u32 = 20;

/// Politeness delay between consecutive search-engine calls. Brave's free
/// tier rate-limits at 1 req/s, so keep this >= 1s when targeting Brave.
const QUERY_DELAY: Duration = Duration::from_millis(1100);

/// One ATS, one set of query templates. The runner sends each query to the
/// search engine, classifies every returned URL, and only counts hits whose
/// `kind` matches `plan.kind` — random unrelated results are silently
/// dropped instead of polluting the DB with the wrong ATS.
pub struct DiscoverPlan {
    pub kind: AtsKind,
    pub queries: &'static [&'static str],
}

// NOTE: Brave's `inurl:` operator silently returns 0 results, so this list
// avoids it. Brave's `site:` semantics also vary by root: it pulls subdomains
// for `recruitee.com` and `myworkdayjobs.com` (most pages live on subdomains)
// but is dominated by the marketing `www.` host for `bamboohr.com`. The
// BambooHR plan uses keyword-rich queries to fish per-tenant pages out from
// under the noise — recall is poor relative to Google CSE and documented as
// such in SOURCES.md.
pub const PLANS: &[DiscoverPlan] = &[
    // Shape A — TLD distinguishes the ATS, `site:` matches everything we want.
    DiscoverPlan {
        kind: AtsKind::Greenhouse,
        queries: &[
            r#"site:boards.greenhouse.io "Remote""#,
            r#"site:job-boards.greenhouse.io "Remote""#,
        ],
    },
    DiscoverPlan {
        kind: AtsKind::Ashbyhq,
        queries: &[r#"site:jobs.ashbyhq.com "Remote""#],
    },
    DiscoverPlan {
        kind: AtsKind::Lever,
        queries: &[r#"site:jobs.lever.co "Remote""#],
    },
    DiscoverPlan {
        kind: AtsKind::Smartrecruiters,
        queries: &[r#"site:jobs.smartrecruiters.com "Remote""#],
    },
    // Shape B — slug is the subdomain. Brave returns subdomains for these
    // two roots without further coaxing.
    DiscoverPlan {
        kind: AtsKind::Recruitee,
        queries: &[r#"site:recruitee.com "Remote""#],
    },
    DiscoverPlan {
        kind: AtsKind::Workday,
        queries: &[r#"site:myworkdayjobs.com "Remote""#],
    },
    // BambooHR — Brave's `site:` collapses to www.bamboohr.com here. Use
    // role-keyword variations to surface per-tenant boards one at a time.
    DiscoverPlan {
        kind: AtsKind::Bamboohr,
        queries: &[
            r#"site:bamboohr.com engineer remote"#,
            r#"site:bamboohr.com developer remote"#,
            r#"site:bamboohr.com software remote"#,
            r#"site:bamboohr.com careers remote"#,
        ],
    },
];

pub fn plan_for(kind_name: &str) -> Option<&'static DiscoverPlan> {
    PLANS.iter().find(|p| p.kind.as_str() == kind_name)
}

#[derive(Debug, Default)]
pub struct DiscoverReport {
    pub kind: &'static str,
    pub queries_sent: u64,
    pub urls_seen: u64,
    pub urls_classified: u64,
    pub companies_new: u64,
    pub companies_seen: u64,
}

pub async fn run_plan(
    db: &Db,
    engine: &dyn SearchEngine,
    plan: &'static DiscoverPlan,
) -> Result<DiscoverReport> {
    let mut report = DiscoverReport {
        kind: plan.kind.as_str(),
        ..Default::default()
    };
    let discovered_via = format!("discover:{}", engine.name());
    for (idx, query) in plan.queries.iter().enumerate() {
        if idx > 0 {
            tokio::time::sleep(QUERY_DELAY).await;
        }
        report.queries_sent += 1;
        info!(kind = plan.kind.as_str(), query = %query, "search");
        let hits = match engine.search(query, RESULTS_PER_QUERY).await {
            Ok(h) => h,
            Err(e) => {
                warn!(error = %e, query = %query, "search failed");
                continue;
            }
        };
        report.urls_seen += hits.len() as u64;
        for hit in hits {
            // Use the strict classifier (not classify_or_other) so we don't
            // upsert generic `Other` rows from any unrelated search noise.
            let Some(ats) = classify_apply_url(&hit.url) else {
                debug!(url = %hit.url, "unclassified");
                continue;
            };
            if ats.kind != plan.kind {
                debug!(expected = plan.kind.as_str(), got = ats.kind.as_str(), url = %hit.url, "kind mismatch");
                continue;
            }
            report.urls_classified += 1;
            match db.upsert_company(None, ats.kind, &ats.slug, &discovered_via, Some(&hit.url)) {
                Ok((_, is_new)) => {
                    report.companies_seen += 1;
                    if is_new {
                        report.companies_new += 1;
                        info!(slug = %ats.slug, kind = ats.kind.as_str(), "new company");
                    }
                }
                Err(e) => warn!(error = %e, slug = %ats.slug, "company upsert failed"),
            }
        }
    }
    Ok(report)
}
