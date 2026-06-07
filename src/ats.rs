use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtsKind {
    Greenhouse,
    Ashbyhq,
    Lever,
    Workable,
    Smartrecruiters,
    Bamboohr,
    Recruitee,
    Personio,
    Breezy,
    Teamtailor,
    Pinpoint,
    Jazzhr,
    Workday,
    Comeet,
    /// Catch-all for jobs whose apply URL is a company's own careers page,
    /// a sub-aggregator (jobs.solana.com, getro, ...), or anything else we
    /// don't recognize as a major ATS. Slug is the URL host so these still
    /// group meaningfully in `list companies`.
    Other,
}

impl AtsKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AtsKind::Greenhouse => "greenhouse",
            AtsKind::Ashbyhq => "ashbyhq",
            AtsKind::Lever => "lever",
            AtsKind::Workable => "workable",
            AtsKind::Smartrecruiters => "smartrecruiters",
            AtsKind::Bamboohr => "bamboohr",
            AtsKind::Recruitee => "recruitee",
            AtsKind::Personio => "personio",
            AtsKind::Breezy => "breezy",
            AtsKind::Teamtailor => "teamtailor",
            AtsKind::Pinpoint => "pinpoint",
            AtsKind::Jazzhr => "jazzhr",
            AtsKind::Workday => "workday",
            AtsKind::Comeet => "comeet",
            AtsKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtsRef {
    pub kind: AtsKind,
    pub slug: String,
    pub external_id: Option<String>,
}

pub fn classify_apply_url(raw: &str) -> Option<AtsRef> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let segs: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();

    // boards.greenhouse.io/{slug}[/jobs/{id}]
    if host == "boards.greenhouse.io" || host == "job-boards.greenhouse.io" {
        let slug = segs.first()?.to_string();
        let external_id = segs
            .iter()
            .position(|s| *s == "jobs")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Greenhouse,
            slug,
            external_id,
        });
    }

    // jobs.ashbyhq.com/{slug}[/{job_uuid}]
    if host == "jobs.ashbyhq.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Ashbyhq,
            slug,
            external_id,
        });
    }

    // jobs.lever.co/{slug}[/{job_uuid}]
    if host == "jobs.lever.co" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Lever,
            slug,
            external_id,
        });
    }

    // apply.workable.com/{slug}[/j/{id}] OR {slug}.workable.com
    if host == "apply.workable.com" || host == "jobs.workable.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs
            .iter()
            .position(|s| *s == "j")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Workable,
            slug,
            external_id,
        });
    }
    if let Some(slug) = host.strip_suffix(".workable.com") {
        return Some(AtsRef {
            kind: AtsKind::Workable,
            slug: slug.to_string(),
            external_id: None,
        });
    }

    // jobs.smartrecruiters.com/{slug}[/{id}]
    if host == "jobs.smartrecruiters.com" || host == "careers.smartrecruiters.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Smartrecruiters,
            slug,
            external_id,
        });
    }

    // {slug}.bamboohr.com/[careers|jobs]/{id}
    //
    // Reject non-tenant subdomains (www marketing, developers docs, etc.)
    // and pages that aren't on a careers/jobs path — both classes leak in
    // when discover/crawl picks up unrelated bamboohr.com URLs.
    if let Some(slug) = host.strip_suffix(".bamboohr.com") {
        if is_non_tenant_subdomain(slug) {
            return None;
        }
        if !segs.iter().any(|s| matches!(*s, "careers" | "jobs")) {
            return None;
        }
        let external_id = segs
            .iter()
            .rev()
            .find(|s| s.chars().all(|c| c.is_ascii_digit()))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Bamboohr,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {slug}.recruitee.com/o/{id-slug}
    //
    // Same defensive checks — www.recruitee.com is the marketing site, and
    // the careers path component (`o`) is required for tenant URLs.
    if let Some(slug) = host.strip_suffix(".recruitee.com") {
        if is_non_tenant_subdomain(slug) {
            return None;
        }
        if !segs.iter().any(|s| matches!(*s, "o" | "careers")) {
            return None;
        }
        let external_id = segs
            .iter()
            .position(|s| *s == "o")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Recruitee,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {slug}.jobs.personio.com or {slug}.jobs.personio.de
    if let Some(rest) = host
        .strip_suffix(".jobs.personio.com")
        .or_else(|| host.strip_suffix(".jobs.personio.de"))
    {
        return Some(AtsRef {
            kind: AtsKind::Personio,
            slug: rest.to_string(),
            external_id: segs.first().map(|s| s.to_string()),
        });
    }

    // {slug}.breezy.hr/p/{id}
    if let Some(slug) = host.strip_suffix(".breezy.hr") {
        let external_id = segs
            .iter()
            .position(|s| *s == "p")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Breezy,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {slug}.teamtailor.com/jobs/{id}
    if let Some(slug) = host.strip_suffix(".teamtailor.com") {
        let external_id = segs
            .iter()
            .position(|s| *s == "jobs")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Teamtailor,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {slug}.pinpointhq.com/[en/]postings/{id}
    if let Some(slug) = host.strip_suffix(".pinpointhq.com") {
        let external_id = segs
            .iter()
            .position(|s| *s == "postings")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Pinpoint,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {slug}.applytojob.com/apply/{id}
    if let Some(slug) = host.strip_suffix(".applytojob.com") {
        let external_id = segs
            .iter()
            .position(|s| *s == "apply")
            .and_then(|i| segs.get(i + 1))
            .map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Jazzhr,
            slug: slug.to_string(),
            external_id,
        });
    }

    // {tenant}.wd{N}.myworkdayjobs.com/[lang/]{site}/job/{loc}/{title}_{id}
    //
    // Workday URLs encode three pieces — tenant, region (wd1..wd12), and the
    // career-site name — which the adapter all needs to hit the JSON
    // endpoint. We pack them as a composite slug `tenant/wd{N}/site` so
    // `companies.ats_slug` stays a single column. Drops anything that
    // looks like a language code (`en-US`, `fr_FR`, `de`) before picking
    // the site name.
    if host.ends_with(".myworkdayjobs.com") {
        let host_parts: Vec<&str> = host.split('.').collect();
        if host_parts.len() >= 4 {
            let tenant = host_parts[0];
            let region = host_parts[1];
            if region.starts_with("wd") && !tenant.is_empty() {
                let site = segs
                    .iter()
                    .find(|s| !is_lang_code(s))
                    .map(|s| s.to_string());
                if let Some(site) = site {
                    let composite = format!("{tenant}/{region}/{site}");
                    let external_id = segs.last().map(|s| s.to_string());
                    return Some(AtsRef {
                        kind: AtsKind::Workday,
                        slug: composite,
                        external_id,
                    });
                }
            }
        }
    }

    // comeet.com/jobs/{slug}/...
    if host == "www.comeet.com" || host == "comeet.com" {
        let i = segs.iter().position(|s| *s == "jobs")?;
        let slug = segs.get(i + 1)?.to_string();
        let external_id = segs.get(i + 3).map(|s| s.to_string());
        return Some(AtsRef {
            kind: AtsKind::Comeet,
            slug,
            external_id,
        });
    }

    None
}

/// True for subdomain prefixes that aren't tenant slugs — `www` (marketing),
/// `developers` (dev docs), `api`, etc. Used to keep search-engine discover
/// runs from upserting bogus rows when an ATS root domain (`bamboohr.com`,
/// `recruitee.com`) also hosts unrelated content under non-tenant subdomains.
fn is_non_tenant_subdomain(slug: &str) -> bool {
    matches!(
        slug,
        "www"
            | "developers"
            | "developer"
            | "api"
            | "docs"
            | "doc"
            | "support"
            | "help"
            | "blog"
            | "store"
            | "shop"
            | "status"
            | "marketing"
            | "about"
            | "info"
    )
}

/// True for segments that look like an i18n locale (`en`, `en-US`, `fr_FR`).
/// Used to skip past language-prefix path segments on Workday job URLs so
/// the next segment, which IS the career-site name, is what gets captured.
fn is_lang_code(s: &str) -> bool {
    match s.len() {
        2 => s.chars().all(|c| c.is_ascii_alphabetic()),
        5 => {
            let bytes = s.as_bytes();
            (bytes[2] == b'-' || bytes[2] == b'_')
                && s[..2].chars().all(|c| c.is_ascii_alphabetic())
                && s[3..].chars().all(|c| c.is_ascii_alphabetic())
        }
        _ => false,
    }
}

/// Detect URLs that are obvious login/signup walls rather than real apply
/// destinations. Used to filter out things like
/// `https://network.bondex.app/auth/login?signup=web3.career&...` where the
/// site has hidden the real employer URL behind a sign-up gate.
pub fn is_auth_wall(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/auth/login")
        || lower.contains("/auth/signup")
        || lower.contains("?signup=")
        || lower.contains("&signup=")
}

/// Classify the apply URL into a known ATS, OR fall back to `AtsKind::Other`
/// with the URL host as slug. Returns `None` if the URL is unparseable, has
/// no host, or is detected as an auth/signup wall.
pub fn classify_or_other(url: &str) -> Option<AtsRef> {
    if is_auth_wall(url) {
        return None;
    }
    if let Some(r) = classify_apply_url(url) {
        return Some(r);
    }
    let u = Url::parse(url).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let external_id = {
        let p = u.path().trim_matches('/');
        if p.is_empty() {
            None
        } else {
            Some(p.to_string())
        }
    };
    Some(AtsRef {
        kind: AtsKind::Other,
        slug: host,
        external_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greenhouse() {
        let r = classify_apply_url("https://boards.greenhouse.io/parity/jobs/4567").unwrap();
        assert_eq!(r.kind, AtsKind::Greenhouse);
        assert_eq!(r.slug, "parity");
        assert_eq!(r.external_id.as_deref(), Some("4567"));
    }

    #[test]
    fn ashby() {
        let r = classify_apply_url("https://jobs.ashbyhq.com/parity/abc-def").unwrap();
        assert_eq!(r.kind, AtsKind::Ashbyhq);
        assert_eq!(r.slug, "parity");
    }

    #[test]
    fn lever() {
        let r = classify_apply_url("https://jobs.lever.co/leverdemo/uuid").unwrap();
        assert_eq!(r.kind, AtsKind::Lever);
    }

    #[test]
    fn workable_apply() {
        let r = classify_apply_url("https://apply.workable.com/circle/j/ABC123").unwrap();
        assert_eq!(r.kind, AtsKind::Workable);
        assert_eq!(r.slug, "circle");
        assert_eq!(r.external_id.as_deref(), Some("ABC123"));
    }

    #[test]
    fn workable_subdomain() {
        let r = classify_apply_url("https://acme.workable.com/").unwrap();
        assert_eq!(r.kind, AtsKind::Workable);
        assert_eq!(r.slug, "acme");
    }

    #[test]
    fn unknown_passes_through() {
        assert!(classify_apply_url("https://example.com/careers").is_none());
    }

    #[test]
    fn other_catches_unknown() {
        let r = classify_or_other("https://jobs.solana.com/companies/ondo-finance/jobs/81464556-x")
            .unwrap();
        assert_eq!(r.kind, AtsKind::Other);
        assert_eq!(r.slug, "jobs.solana.com");
        assert_eq!(
            r.external_id.as_deref(),
            Some("companies/ondo-finance/jobs/81464556-x")
        );
    }

    #[test]
    fn other_returns_none_for_bad_url() {
        assert!(classify_or_other("mailto:hr@acme.com").is_none());
    }

    #[test]
    fn bamboohr_rejects_marketing_subdomain() {
        // www.bamboohr.com is the marketing site, not a tenant.
        assert!(
            classify_apply_url("https://www.bamboohr.com/careers/engineering-it-team").is_none()
        );
        // developers.bamboohr.com is dev docs.
        assert!(classify_apply_url("https://developers.bamboohr.com/jobs/anything").is_none());
    }

    #[test]
    fn bamboohr_requires_careers_path() {
        // A tenant subdomain on an unrelated path should not classify.
        assert!(classify_apply_url("https://sololearn.bamboohr.com/about").is_none());
        // Tenant with a real jobs path classifies cleanly.
        let r = classify_apply_url("https://sololearn.bamboohr.com/jobs/view.php").unwrap();
        assert_eq!(r.kind, AtsKind::Bamboohr);
        assert_eq!(r.slug, "sololearn");
    }

    #[test]
    fn recruitee_rejects_marketing_and_requires_offer_path() {
        assert!(classify_apply_url("https://www.recruitee.com/features").is_none());
        let r =
            classify_apply_url("https://exeon.recruitee.com/o/senior-backend-developer").unwrap();
        assert_eq!(r.kind, AtsKind::Recruitee);
        assert_eq!(r.slug, "exeon");
    }

    #[test]
    fn workday_composite_slug() {
        let r = classify_apply_url(
            "https://nvidia.wd5.myworkdayjobs.com/en-US/NVIDIAExternalCareerSite/job/Israel-Yokneam/Engineer_JR2016630",
        )
        .unwrap();
        assert_eq!(r.kind, AtsKind::Workday);
        assert_eq!(r.slug, "nvidia/wd5/NVIDIAExternalCareerSite");
        assert_eq!(r.external_id.as_deref(), Some("Engineer_JR2016630"));
    }

    #[test]
    fn workday_no_lang_prefix() {
        let r = classify_apply_url(
            "https://salesforce.wd1.myworkdayjobs.com/External_Career_Site/job/CA-San-Francisco/Eng_JR0987",
        )
        .unwrap();
        assert_eq!(r.slug, "salesforce/wd1/External_Career_Site");
    }

    #[test]
    fn workday_missing_site_rejected() {
        // Bare host with no site segment in the path — can't be synced.
        assert!(classify_apply_url("https://nvidia.wd5.myworkdayjobs.com/").is_none());
    }

    #[test]
    fn is_lang_code_basic() {
        assert!(is_lang_code("en"));
        assert!(is_lang_code("en-US"));
        assert!(is_lang_code("fr_FR"));
        assert!(!is_lang_code("External"));
        assert!(!is_lang_code("NVIDIAExternalCareerSite"));
        assert!(!is_lang_code("wd5"));
    }

    #[test]
    fn auth_wall_skipped() {
        let url = "https://network.bondex.app/auth/login?signup=web3.career&utm_source=web3.career";
        assert!(is_auth_wall(url));
        assert!(classify_or_other(url).is_none());
    }
}
