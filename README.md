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
    mod.rs           # trait Crawler + registry (`all()`, `by_name()`)
    web3career.rs    # crawler for https://web3.career/remote-jobs
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

## Status (MVP)

- Builds, tests pass (6 unit tests for the ATS URL classifier).
- `web3career` crawler fetches the page and harvests outbound `<a href>` links
  through the classifier. **Currently finds 0 ATS matches** because
  web3.career routes apply links through internal `/apply/{id}` redirects
  rather than linking directly to ATS boards. Next iteration: either follow
  detail pages, or start with a site that exposes direct ATS links.
- No ATS adapters yet — once we have company slugs, the next phase is to add
  per-ATS adapters (Greenhouse, Ashby, Lever) that read
  `companies WHERE ats_kind=?` and pull the full job list via the ATS JSON API.

## Roadmap (short)

1. Get one crawler producing real `(ats_kind, ats_slug)` rows
   (candidates: `cryptocurrencyjobs.co`, `rustjobs.dev`, HN Who's Hiring).
2. Add a Greenhouse adapter (`boards-api.greenhouse.io/v1/boards/{slug}/jobs`)
   that populates the `jobs` table for known slugs.
3. Add Ashby and Lever adapters (same pattern, different JSON shape).
4. Add a `discover` command that does search-engine queries
   (`site:boards.greenhouse.io ...`) via Google CSE for slug discovery.
