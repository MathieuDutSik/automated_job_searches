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

```sh
# Run a single crawler
ajs crawl web3career

# Run every registered crawler in sequence
ajs crawl all

# Inspect the database
ajs list companies --limit 50
ajs list jobs --limit 50
ajs status

# Pick a different database file
ajs --db /path/to/jobs.db crawl all
```

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
  crawlers/
    mod.rs                   # trait Crawler + registry (`all()`, `by_name()`)
    cryptocurrencyjobs.rs    # RSS feed + per-detail-page apply URL extraction
    hn_whoshiring.rs         # Algolia API, latest "Who is hiring?" thread
    web3career.rs            # listing + detail crawl (gated by Bondex auth wall)
```

Each crawler implements:

```rust
#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}
```

To add a new crawler: create `src/crawlers/<name>.rs`, add it to the `vec![...]`
returned by `crawlers::all()` in `src/crawlers/mod.rs`, and have its `run()`
extract ATS URLs (or HTML it can parse into `(company, ats_kind, ats_slug)`
tuples) and call `db.upsert_company(...)`. Once registered, it's picked up by
both `ajs crawl <name>` and `ajs crawl all`.

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

1. **Greenhouse adapter** — `https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true`.
   First ATS adapter; cleanest public API.
2. **Ashby adapter** — `https://api.ashbyhq.com/posting-api/job-board/{slug}`.
3. **Lever adapter** — `https://api.lever.co/v0/postings/{slug}?mode=json`.
4. **`discover` command** — Google Custom Search Engine queries like
   `site:boards.greenhouse.io "engineer" "remote"` to find new ATS slugs
   beyond what crawlers surface.
5. **More crawlers** — `weworkremotely.com`, `remote.co`, `jobspresso.co`,
   getro VC-portfolio boards.
6. **Auth-walled sources** — cookie-paste support for web3.career; revisit
   rustjobs.dev (Vercel JS challenge) with a headless browser if it's worth it.
