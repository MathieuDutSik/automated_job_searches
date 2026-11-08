//! Rule-based job filter (`ajs filter`, and automatically after
//! crawl / sync / import / resolve).
//!
//! Rules live in `filters.toml` (see the comments there). Every open job
//! gets a verdict in the `job_filter` table — kept, or excluded with the
//! first rule it failed as the reason — and `ajs list jobs` hides the
//! excluded ones. Verdicts are kept apart from `jobs` so writing them
//! doesn't touch the FTS index, and apart from the manual `status` so
//! `ajs mark` stays yours.

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::OnceLock;
use tracing::info;

use crate::db::{Db, FilterInput};

/// Bump when the evaluation logic changes, so stored verdicts are redone
/// even if `filters.toml` didn't change.
const LOGIC_VERSION: &str = "1";

#[derive(Deserialize)]
struct Config {
    title: TitleConfig,
    remote: RemoteConfig,
    region: RegionConfig,
    salary: SalaryConfig,
}

#[derive(Deserialize)]
struct TitleConfig {
    keep: Vec<String>,
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct RemoteConfig {
    unknown_ok_if: Vec<String>,
}

#[derive(Deserialize)]
struct RegionConfig {
    allow: Vec<String>,
    block: Vec<String>,
    description_restrictions: Vec<String>,
}

#[derive(Deserialize)]
struct SalaryConfig {
    min_usd: f64,
    fx_to_usd: HashMap<String, f64>,
}

pub struct Rules {
    title_keep: Regex,
    title_exclude: Regex,
    remote_text: Regex,
    region_allow: Regex,
    region_block: Regex,
    region_desc: Vec<Regex>,
    min_usd: f64,
    fx: HashMap<String, f64>,
    /// Hash of the config text and LOGIC_VERSION, stored in `meta` to tell
    /// whether stored verdicts are current.
    pub fingerprint: String,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Keep,
    Exclude(String),
}

impl Rules {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        LOGIC_VERSION.hash(&mut h);
        let block_alt = alternation(&cfg.region.block);
        let region_desc = cfg
            .region
            .description_restrictions
            .iter()
            .map(|p| ci_regex(&p.replace("{BLOCK}", &format!("(?:{block_alt})"))))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            title_keep: ci_regex(&bounded(&cfg.title.keep))?,
            title_exclude: ci_regex(&bounded(&cfg.title.exclude))?,
            remote_text: ci_regex(&bounded(&cfg.remote.unknown_ok_if))?,
            region_allow: ci_regex(&format!(r"\b(?:{})\b", alternation(&cfg.region.allow)))?,
            region_block: ci_regex(&format!(r"\b(?:{block_alt})\b"))?,
            region_desc,
            min_usd: cfg.salary.min_usd,
            fx: cfg
                .salary
                .fx_to_usd
                .into_iter()
                .map(|(k, v)| (k.to_ascii_uppercase(), v))
                .collect(),
            fingerprint: format!("{:016x}", h.finish()),
        })
    }

    pub fn evaluate(&self, job: &FilterInput) -> Verdict {
        let title = job.title.as_str();
        if let Some(m) = self.title_exclude.find(title) {
            return Verdict::Exclude(format!("title: {}", m.as_str().to_lowercase()));
        }
        if !self.title_keep.is_match(title) {
            return Verdict::Exclude("title: not a target role".to_string());
        }

        let location = job.location.as_deref().unwrap_or("");
        let place = format!("{location} | {title}");
        let remote = match job.remote {
            Some(r) => r,
            None => self.remote_text.is_match(&place),
        };
        if !remote {
            return Verdict::Exclude("not remote".to_string());
        }

        if !self.region_allow.is_match(&place) {
            if let Some(m) = self.region_block.find(&place) {
                return Verdict::Exclude(format!("region: {}", m.as_str()));
            }
            if let Some(desc) = job.description.as_deref() {
                for re in &self.region_desc {
                    if let Some(m) = re.find(desc) {
                        return Verdict::Exclude(format!("region (description): {}", m.as_str()));
                    }
                }
            }
        }

        if let Some(top) = self.salary_top_usd(job) {
            if top < self.min_usd {
                return Verdict::Exclude(format!("salary: up to ${}k/yr", (top / 1000.0).round()));
            }
        }
        Verdict::Keep
    }

    /// Highest annual figure, in USD, among the salaries stated for the
    /// job. Taking the highest keeps a stipend or a bonus mentioned next to
    /// the real salary from excluding the job.
    fn salary_top_usd(&self, job: &FilterInput) -> Option<f64> {
        let mut amounts: Vec<Money> = Vec::new();
        if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&job.raw_json) {
            amounts.extend(structured_salary(&raw));
        }
        if let Some(desc) = job.description.as_deref() {
            amounts.extend(salaries_in_text(desc, true));
        }
        // HN posts carry the salary in the title line ("… | $65k-$85k + equity").
        amounts.extend(salaries_in_text(&job.title, false));
        amounts
            .into_iter()
            .filter_map(|m| {
                let rate = *self.fx.get(&m.currency)?;
                Some(m.annual * rate)
            })
            .filter(|v| (5_000.0..3_000_000.0).contains(v))
            .reduce(f64::max)
    }
}

/// Patterns joined with `|`; short all-caps terms (US, EMEA, ...) are
/// wrapped to match case-sensitively inside the case-insensitive regex.
fn alternation(terms: &[String]) -> String {
    terms
        .iter()
        .map(|t| {
            let caps = t.chars().all(|c| c.is_ascii_uppercase()) && t.len() <= 5;
            if caps {
                format!("(?-i:{t})")
            } else {
                format!("(?:{t})")
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn bounded(terms: &[String]) -> String {
    format!(r"\b(?:{})\b", alternation(terms))
}

fn ci_regex(pattern: &str) -> Result<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 24)
        .build()
        .with_context(|| format!("bad pattern: {pattern}"))
}

// --- salary parsing --------------------------------------------------------

#[derive(Debug, PartialEq)]
struct Money {
    currency: String,
    /// Upper bound, per year, in `currency`.
    annual: f64,
}

/// Salary fields some ATSes return as data.
fn structured_salary(raw: &serde_json::Value) -> Vec<Money> {
    let mut out = Vec::new();
    // Lever: {"salaryRange": {"currency", "interval", "min", "max"}}
    if let Some(r) = raw.get("salaryRange") {
        let cur = r.get("currency").and_then(|v| v.as_str());
        let max = r.get("max").and_then(|v| v.as_f64());
        let per_year = match r.get("interval").and_then(|v| v.as_str()) {
            Some("per-year-salary") => Some(1.0),
            Some("per-month-salary") => Some(12.0),
            Some("bi-week-salary") => Some(26.0),
            Some("per-week-salary") => Some(52.0),
            Some("per-hour-wage") => Some(2080.0),
            _ => None,
        };
        if let (Some(cur), Some(max), Some(k)) = (cur, max, per_year) {
            out.push(Money {
                currency: cur.to_ascii_uppercase(),
                annual: max * k,
            });
        }
    }
    // Ashby: compensation.scrapeableCompensationSalarySummary, e.g. "€90K - €160K"
    if let Some(s) = raw
        .pointer("/compensation/scrapeableCompensationSalarySummary")
        .and_then(|v| v.as_str())
    {
        out.extend(salaries_in_text(s, false));
    }
    out
}

/// Money amounts in free text that look like pay. With `need_context`,
/// an amount counts only next to a pay word or a pay period, so funding
/// rounds, budgets and prices are ignored.
fn salaries_in_text(text: &str, need_context: bool) -> Vec<Money> {
    static AMOUNT: OnceLock<Regex> = OnceLock::new();
    static CONTEXT: OnceLock<Regex> = OnceLock::new();
    let amount = AMOUNT.get_or_init(|| {
        let cur = r"[$€£]|USD|EUR|GBP|CHF|CAD|AUD|NZD|SEK|NOK|DKK|PLN|INR|BRL|MXN";
        let num = r"\d{1,3}(?:[,.\u{a0} ]\d{3})+|\d+(?:\.\d+)?";
        Regex::new(&format!(
            r"(?P<c1>{cur})\s?(?P<a>{num})\s?(?P<ka>[kK]\b)?(?:\s*(?:-|–|—|to)\s*(?:{cur})?\s?(?P<b>{num})\s?(?P<kb>[kK]\b)?)?(?P<c2>\s?(?:{cur}))?"
        ))
        .unwrap()
    });
    let context = CONTEXT.get_or_init(|| {
        Regex::new(r"(?i)salary|compensation|\bpay\b|\bpaid\b|base|\bote\b|annual|per\s+(year|annum|month|hour|week)|/\s?(yr|year|mo|month|h|hr|hour)\b|\ba\s+year\b|gross|wage|rate")
            .unwrap()
    });
    static SCALE: OnceLock<Regex> = OnceLock::new();
    let scale =
        SCALE.get_or_init(|| Regex::new(r"^\s?(?:m|mm|mn|million|b|bn|billion)\b").unwrap());
    let mut out = Vec::new();
    for caps in amount.captures_iter(text) {
        let whole = caps.get(0).unwrap();
        let after = tail(text, whole.end(), 40);
        let after_l = after.to_ascii_lowercase();
        // "$5M", "$40 million", "$5B raised", ...
        if scale.is_match(&after_l) {
            continue;
        }
        let before = head(text, whole.start(), 80);
        let near = format!("{before} {} {after}", whole.as_str());
        if need_context && !context.is_match(&near) {
            continue;
        }
        let currency = currency_code(caps.name("c1").map(|m| m.as_str()).unwrap_or(""));
        let (upper, k) = match caps.name("b") {
            Some(b) => (b.as_str(), caps.name("kb").or(caps.name("ka")).is_some()),
            None => (caps.name("a").unwrap().as_str(), caps.name("ka").is_some()),
        };
        let Some(mut value) = parse_number(upper) else { continue };
        if k {
            value *= 1000.0;
        }
        let per_year = period_multiplier(&after_l);
        // A small monthly or weekly amount is a stipend, not the salary.
        if per_year > 1.0 && per_year < 2080.0 && value < 800.0 {
            continue;
        }
        // A bare small number with no period ("$50 gift card") isn't pay.
        if per_year == 1.0 && value < 1000.0 {
            continue;
        }
        out.push(Money {
            currency,
            annual: value * per_year,
        });
    }
    out
}

fn period_multiplier(after_lower: &str) -> f64 {
    static PERIOD: OnceLock<Regex> = OnceLock::new();
    let re = PERIOD.get_or_init(|| {
        Regex::new(r"^\W{0,3}(?:usd|eur|gbp)?\s*(?:/|per|an|a|each)?\s*(?P<p>hour|hr|h\b|month|mo\b|week|wk|day|year|yr|annum)")
            .unwrap()
    });
    match re.captures(after_lower).and_then(|c| c.name("p")).map(|m| m.as_str()) {
        Some("hour" | "hr" | "h") => 2080.0,
        Some("month" | "mo") => 12.0,
        Some("week" | "wk") => 52.0,
        Some("day") => 220.0,
        _ => 1.0,
    }
}

fn currency_code(sym: &str) -> String {
    match sym {
        "$" => "USD".into(),
        "€" => "EUR".into(),
        "£" => "GBP".into(),
        other => other.to_ascii_uppercase(),
    }
}

/// "120,000" / "120.000" / "120 000" → 120000; "6.5" → 6.5.
fn parse_number(s: &str) -> Option<f64> {
    static GROUPED: OnceLock<Regex> = OnceLock::new();
    let grouped =
        GROUPED.get_or_init(|| Regex::new(r"^\d{1,3}(?:[,.\u{a0} ]\d{3})+$").unwrap());
    if grouped.is_match(s) {
        s.chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    } else {
        s.parse().ok()
    }
}

fn head(text: &str, end: usize, max: usize) -> &str {
    let mut start = end.saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..end]
}

fn tail(text: &str, start: usize, max: usize) -> &str {
    let mut end = (start + max).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[start..end]
}

// --- applying to the database -----------------------------------------------

#[derive(Debug, Default)]
pub struct FilterReport {
    pub scored: u64,
    pub kept: u64,
    pub excluded: u64,
    /// Exclusions by rule (the part of the reason before `:`).
    pub by_rule: BTreeMap<String, u64>,
}

/// Score open jobs. With `force`, or when the rules changed since the
/// last run, every open job is re-scored; otherwise only jobs without a
/// verdict yet.
pub fn apply(db: &Db, rules: &Rules, force: bool) -> Result<FilterReport> {
    let stale = db.get_meta("filter_fingerprint")?.as_deref() != Some(rules.fingerprint.as_str());
    let all = force || stale;
    let jobs = db.jobs_for_filter(all)?;
    let mut report = FilterReport::default();
    let mut verdicts = Vec::with_capacity(jobs.len());
    for job in &jobs {
        let v = rules.evaluate(job);
        report.scored += 1;
        match &v {
            Verdict::Keep => report.kept += 1,
            Verdict::Exclude(reason) => {
                report.excluded += 1;
                let rule = reason.split(':').next().unwrap_or(reason).to_string();
                *report.by_rule.entry(rule).or_default() += 1;
            }
        }
        verdicts.push((job.id, v));
    }
    db.store_filter_verdicts(&verdicts)?;
    db.set_meta("filter_fingerprint", &rules.fingerprint)?;
    info!(scored = report.scored, kept = report.kept, excluded = report.excluded, all, "filter applied");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/filters.toml"))
            .unwrap();
        Rules::parse(&text).unwrap()
    }

    fn job(title: &str, location: &str, remote: Option<bool>, desc: &str) -> FilterInput {
        FilterInput {
            id: 1,
            title: title.into(),
            location: Some(location.into()),
            remote,
            description: Some(desc.into()),
            raw_json: "{}".into(),
        }
    }

    fn reason(j: &FilterInput) -> String {
        match rules().evaluate(j) {
            Verdict::Keep => "keep".into(),
            Verdict::Exclude(r) => r,
        }
    }

    #[test]
    fn titles() {
        assert_eq!(reason(&job("Senior Rust Engineer", "Remote", Some(true), "")), "keep");
        assert_eq!(reason(&job("Engineering Manager", "Remote", Some(true), "")), "title: manager");
        assert_eq!(reason(&job("Growth Hacker", "Remote", Some(true), "")), "title: growth");
        assert_eq!(reason(&job("Operations Research Scientist", "Remote", Some(true), "")), "keep");
        assert_eq!(
            reason(&job("Office Coordinator", "Remote", Some(true), "")),
            "title: not a target role"
        );
        // Word boundaries: "intern" must not hit "international".
        assert_eq!(reason(&job("Software Engineer, International", "Remote", Some(true), "")), "keep");
    }

    #[test]
    fn remote_and_region() {
        assert_eq!(reason(&job("Software Engineer", "Berlin", Some(false), "")), "not remote");
        assert_eq!(reason(&job("Software Engineer", "Remote - US", Some(true), "")), "region: US");
        assert_eq!(reason(&job("Software Engineer", "Spain, Mexico", Some(true), "")), "keep");
        assert_eq!(reason(&job("Software Engineer", "Remote, EMEA", Some(true), "")), "keep");
        // Lowercase "us" in text is a word, not the country.
        assert_eq!(
            reason(&job("Software Engineer", "Remote", Some(true), "Join us to build compilers.")),
            "keep"
        );
        assert!(reason(&job(
            "Software Engineer",
            "Remote",
            Some(true),
            "Candidates must be based in the United States."
        ))
        .starts_with("region (description)"));
        assert!(reason(&job("Software Engineer", "Remote", Some(true), "This role is LATAM-only."))
            .starts_with("region (description)"));
        // Unknown remote flag falls back to the location text.
        assert_eq!(reason(&job("Software Engineer", "Remote (Europe)", None, "")), "keep");
        assert_eq!(reason(&job("Software Engineer", "Paris", None, "")), "not remote");
    }

    #[test]
    fn salaries() {
        let r = |d: &str| reason(&job("Software Engineer", "Remote, Europe", Some(true), d));
        assert_eq!(r("Compensation: $6,500 USD per month."), "salary: up to $78k/yr");
        assert_eq!(r("What we offer: up to $10/hour, 100% remote"), "salary: up to $21k/yr");
        assert_eq!(r("Salary range: €60.000 - €75.000 gross per year"), "salary: up to $81k/yr");
        assert_eq!(r("Base salary $120k-$160k"), "keep");
        // Not pay: funding, ad spend, investment.
        assert_eq!(r("Our clients have raised over $5B. We spend $40 Million a year on ads."), "keep");
        assert_eq!(r("We provide an initial £250k investment."), "keep");
        // A stipend next to a real salary doesn't win.
        assert_eq!(r("Salary $150,000. Plus a $500/month home office stipend."), "keep");
        let hn = job("Pairio | Senior Swift Engineer | REMOTE (Europe) | $65k-$85k + equity", "", None, "");
        assert_eq!(reason(&hn), "salary: up to $85k/yr");
    }

    #[test]
    fn structured_salaries() {
        let lever = serde_json::json!({"salaryRange": {"currency": "USD", "interval": "per-year-salary", "min": 60000, "max": 80000}});
        assert_eq!(structured_salary(&lever)[0].annual, 80000.0);
        let ashby = serde_json::json!({"compensation": {"scrapeableCompensationSalarySummary": "€90K - €160K"}});
        let m = &structured_salary(&ashby)[0];
        assert_eq!((m.currency.as_str(), m.annual), ("EUR", 160000.0));
    }
}
