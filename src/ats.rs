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
        let external_id = segs.iter().position(|s| *s == "jobs").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Greenhouse, slug, external_id });
    }

    // jobs.ashbyhq.com/{slug}[/{job_uuid}]
    if host == "jobs.ashbyhq.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Ashbyhq, slug, external_id });
    }

    // jobs.lever.co/{slug}[/{job_uuid}]
    if host == "jobs.lever.co" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Lever, slug, external_id });
    }

    // apply.workable.com/{slug}[/j/{id}] OR {slug}.workable.com
    if host == "apply.workable.com" || host == "jobs.workable.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs.iter().position(|s| *s == "j").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Workable, slug, external_id });
    }
    if let Some(slug) = host.strip_suffix(".workable.com") {
        return Some(AtsRef { kind: AtsKind::Workable, slug: slug.to_string(), external_id: None });
    }

    // jobs.smartrecruiters.com/{slug}[/{id}]
    if host == "jobs.smartrecruiters.com" || host == "careers.smartrecruiters.com" {
        let slug = segs.first()?.to_string();
        let external_id = segs.get(1).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Smartrecruiters, slug, external_id });
    }

    // {slug}.bamboohr.com/[careers|jobs]/{id}
    if let Some(slug) = host.strip_suffix(".bamboohr.com") {
        let external_id = segs.iter().rev().find(|s| s.chars().all(|c| c.is_ascii_digit())).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Bamboohr, slug: slug.to_string(), external_id });
    }

    // {slug}.recruitee.com/o/{id-slug}
    if let Some(slug) = host.strip_suffix(".recruitee.com") {
        let external_id = segs.iter().position(|s| *s == "o").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Recruitee, slug: slug.to_string(), external_id });
    }

    // {slug}.jobs.personio.com or {slug}.jobs.personio.de
    if let Some(rest) = host.strip_suffix(".jobs.personio.com").or_else(|| host.strip_suffix(".jobs.personio.de")) {
        return Some(AtsRef { kind: AtsKind::Personio, slug: rest.to_string(), external_id: segs.first().map(|s| s.to_string()) });
    }

    // {slug}.breezy.hr/p/{id}
    if let Some(slug) = host.strip_suffix(".breezy.hr") {
        let external_id = segs.iter().position(|s| *s == "p").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Breezy, slug: slug.to_string(), external_id });
    }

    // {slug}.teamtailor.com/jobs/{id}
    if let Some(slug) = host.strip_suffix(".teamtailor.com") {
        let external_id = segs.iter().position(|s| *s == "jobs").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Teamtailor, slug: slug.to_string(), external_id });
    }

    // {slug}.pinpointhq.com/[en/]postings/{id}
    if let Some(slug) = host.strip_suffix(".pinpointhq.com") {
        let external_id = segs.iter().position(|s| *s == "postings").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Pinpoint, slug: slug.to_string(), external_id });
    }

    // {slug}.applytojob.com/apply/{id}
    if let Some(slug) = host.strip_suffix(".applytojob.com") {
        let external_id = segs.iter().position(|s| *s == "apply").and_then(|i| segs.get(i + 1)).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Jazzhr, slug: slug.to_string(), external_id });
    }

    // *.{wd_n}.myworkdayjobs.com/{tenant}/job/{...}
    if host.ends_with(".myworkdayjobs.com") {
        let slug = host.split('.').next().unwrap_or("").to_string();
        let external_id = segs.last().map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Workday, slug, external_id });
    }

    // comeet.com/jobs/{slug}/...
    if host == "www.comeet.com" || host == "comeet.com" {
        let i = segs.iter().position(|s| *s == "jobs")?;
        let slug = segs.get(i + 1)?.to_string();
        let external_id = segs.get(i + 3).map(|s| s.to_string());
        return Some(AtsRef { kind: AtsKind::Comeet, slug, external_id });
    }

    None
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
}
