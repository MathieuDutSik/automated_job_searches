# automated_job_searches

CLI tool that crawls job aggregators, discovers companies hosted on known ATS
platforms (Greenhouse, Ashby, Lever, Workable, ...), and stores everything in
a local SQLite file for inspection and re-crawling.

## Build

```sh
cargo build --release
```

The binary is `target/release/ajs`. For dev use, `cargo run --` works the same.

## Usage

There are two phases. Crawlers **discover** which company is hosted on which
ATS (Greenhouse, Ashby, Lever, ...). Adapters then **sync** the full live job
list for every known company from the ATS's public JSON API.

```sh
# Phase 1: discover companies + a sample of jobs
ajs crawl cryptocurrencyjobs      # one crawler
ajs crawl all                     # every registered crawler

# Phase 2: refresh full job lists from the ATS JSON APIs
ajs sync greenhouse               # one ATS (iterates every greenhouse company in DB)
ajs sync all                      # every registered adapter

# Inspect the data
ajs list companies --limit 50           # flat list of companies
ajs list jobs --limit 50                # flat list of open jobs
ajs list by-company --limit 1000        # grouped: each company with its jobs indented
ajs status                              # DB path + totals

# Pick a different database file
ajs --db /path/to/jobs.db crawl all
```

The typical workflow is `crawl all` once to populate `companies`, then
`sync all` whenever you want fresh job lists (idempotent — re-runs update
`last_seen` and mark disappeared jobs `closed_at`).

The default database path is `<repo>/jobs.db` — the absolute path to this
checkout is baked into the binary at build time, so `ajs` writes to the same
file no matter what directory you run it from. Rebuild after moving the
checkout, or pass `--db` explicitly.

Logging is via `RUST_LOG`. Default is `info`. For more detail:

```sh
RUST_LOG=debug ajs crawl web3career
```

## What it stores

A SQLite file (default `<repo>/jobs.db`) with:

- `companies` — one row per `(ats_kind, ats_slug)`. Display name + first/last
  seen timestamps.
- `company_discoveries` — append-only log of which crawler saw which company,
  when, and on what page.
- `jobs` — one row per `(ats_kind, external_id)`. Title, location, apply URL,
  raw API blob, plus `first_seen`/`last_seen`/`closed_at` for lifecycle
  tracking. (Not populated yet — see *Status* below.)
- `crawl_runs` — one row per crawler invocation, with counts and errors. Use
  it to spot which sources are healthy.

The schema is created on first open; re-running is idempotent.

## Layout

```
src/
  main.rs            # clap entrypoint: crawl | list | status
  db.rs              # schema + upsert helpers
  http.rs            # shared reqwest client (UA, timeout, gzip/brotli)
  ats.rs             # classify_apply_url() — recognizes 14 ATS URL patterns
  crawlers/                  # discovery: find (company, ats_kind, ats_slug)
    mod.rs                   # trait Crawler + registry
    cryptocurrencyjobs.rs    # RSS feed + per-detail-page apply URL extraction
    hn_whoshiring.rs         # Algolia API, latest "Who is hiring?" thread
    web3career.rs            # listing + detail crawl (gated by Bondex auth wall)
  adapters/                  # sync: for a known slug, pull all live jobs from ATS API
    mod.rs                   # trait AtsAdapter + registry + sync_all_for_kind()
    greenhouse.rs            # boards-api.greenhouse.io/v1/boards/{slug}/jobs
    ashby.rs                 # api.ashbyhq.com/posting-api/job-board/{slug}
    lever.rs                 # api.lever.co/v0/postings/{slug}?mode=json
```

Crawler trait:

```rust
#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}
```

Adapter trait:

```rust
#[async_trait(?Send)]
pub trait AtsAdapter {
    fn kind(&self) -> AtsKind;
    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>>;
}
```

To add a new crawler or adapter: create the file, register it in the
matching `all()` registry, and (for adapters) implement `fetch_jobs` for one
slug — the shared `sync_all_for_kind()` handles iteration, the politeness
delay, upserting, 404 handling, and the close-unseen-jobs sweep.

## Status

- 14 unit tests pass.
- Three crawlers wired up:
  - **`cryptocurrencyjobs`** — fetches the RSS feed at `/index.xml`, then for
    each item fetches the detail page and extracts the apply URL via the
    `?ref=cryptocurrencyjobs.co` marker. Typically produces ~70-80 jobs/run.
  - **`hn_whoshiring`** — finds the latest *Ask HN: Who is hiring?* thread via
    the Algolia search API, fetches all top-level comments, extracts URLs
    from comment HTML, prefers known-ATS URLs over generic `Other`.
    Typically produces ~200+ jobs/run.
  - **`web3career`** — parked but kept. Apply URLs are gated behind a
    `network.bondex.app` sign-up wall; the `is_auth_wall()` filter (in
    `ats.rs`) correctly rejects them, so this crawler now contributes 0 rows
    instead of polluting the DB. Re-enable later via cookie-paste auth or a
    headless browser if needed.
- The `Other` ATS kind catches anything not matching a known ATS — company
  careers pages, sub-aggregators (`jobs.solana.com`, `careers.smartrecruiters.com`,
  EU Greenhouse mirrors, ...). These aren't dropped — they're stored with the
  URL host as slug so you can still see them in `list jobs`.
- **No ATS adapters yet.** Now that we have real `(ats_kind, ats_slug)` rows
  for Greenhouse / Ashby / Lever / Workable / Breezy / Jazzhr / ..., the next
  phase is to add per-ATS adapters that read `companies WHERE ats_kind=?` and
  pull the full job list via the ATS JSON API for richer/fresher data.

## Roadmap

1. **More ATS adapters** — Workable, Breezy, Smartrecruiters, Recruitee,
   Personio, JazzHR, Teamtailor, BambooHR, Pinpoint. Same pattern as the
   existing three; each is ~50 lines.
2. **`discover` command** — Google Custom Search Engine queries like
   `site:boards.greenhouse.io "engineer" "remote"` to find new ATS slugs
   beyond what crawlers surface.
3. **More crawlers** — `weworkremotely.com`, `remote.co`, `jobspresso.co`,
   getro VC-portfolio boards.
4. **Auth-walled sources** — cookie-paste support for web3.career; revisit
   rustjobs.dev (Vercel JS challenge) with a headless browser if worth it.
5. **Workday adapter** — separate effort. Workday uses per-tenant POST
   endpoints with anti-bot, so it'll need careful work.
