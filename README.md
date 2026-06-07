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

# Filter open jobs by remote flag and/or full-text keywords
ajs list jobs --remote                          # only ATS-flagged remote roles
ajs list jobs --match rust                      # title/loc/dept/description match
ajs list jobs --remote --match 'rust OR zig'    # boolean operators work
ajs list jobs --match '"c++"'                   # quote terms with punctuation

# Per-user job status (your own pipeline state, separate from ATS open/closed)
ajs mark 4130 applied --note "via referral"     # tag applied
ajs mark 3154 dismissed --note "stack mismatch" # hide from default listings
ajs mark 4130 reset                             # back to `new`
ajs list jobs --applied                         # only your applied rows
ajs list jobs --all                             # include `dismissed` again
```

### Finding the id for `ajs mark`

`<id>` is the integer in the **first column** of `ajs list jobs` — the
`jobs.id` SQLite primary key. Stable across crawls, syncs, and restarts;
assigned once and never reused. **Not** the apply URL.

```
   261  LiveKit | Staff Rust SDK Engineer [remote] | … | https://jobs.ashbyhq.com/livekit/a1d10340-…
  4130  Binance | Senior QA Engineer, Margin (Rust/Java) [remote] | Asia | https://jobs.lever.co/binance/1b69b321-…
   ^^^^
   |
   `--- this is what you pass to `ajs mark`
```

So `ajs mark 4130 applied` tags the Binance row; the URL is just shown so
you can click through. If you only know the URL, look it up with one of:

```sh
ajs list jobs --match binance | grep 1b69b321  # quick visual grep on URL fragment
sqlite3 jobs.db "SELECT id FROM jobs WHERE apply_url = 'https://…';"

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
  description (plain text), `remote` flag, raw API blob, plus
  `first_seen`/`last_seen`/`closed_at` for ATS-side lifecycle, and
  `status` (`new` | `applied` | `dismissed`) + `status_changed_at` /
  `status_note` for your own pipeline state. `closed_at` is what the ATS says;
  `status` is what you say — they're independent.
- `jobs_fts` — FTS5 virtual table mirroring `title`/`location`/`department`/
  `description`, kept in sync by triggers. Powers `list jobs --match <query>`.
  Tokenizer is `unicode61 tokenchars '+#.'` — words are split on whitespace
  and punctuation **except** `+`, `#`, `.`, so `c++` / `c#` / `.net` index as
  single tokens (quote them as phrases when querying), while `rust` matches
  only the word `rust` and not `trusted`/`trust`. Separators include space,
  `/`, `-`, `,`, `(`, `)`, etc. — anything not a word character or in
  `tokenchars`.
- `crawl_runs` — one row per crawler invocation, with counts and errors. Use
  it to spot which sources are healthy.
- `meta` — internal key/value (currently tracks the FTS5 build version, so
  a schema bump triggers one automatic rebuild on next open).

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
    cryptojobs.rs            # cryptojobs.com RSS feed (structured externalApplicationLink)
    hn_whoshiring.rs         # Algolia API, latest "Who is hiring?" thread
    thehub.rs                # thehub.io listing + detail page apply-URL extraction
    web3career.rs            # listing + detail crawl (gated by Bondex auth wall)
    workingnomads.rs         # workingnomads.com JSON API + redirect-follow to real apply URL
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

- 19 unit tests pass.
- Six crawlers wired up:
  - **`cryptocurrencyjobs`** — fetches the RSS feed at `/index.xml`, then for
    each item fetches the detail page and extracts the apply URL via the
    `?ref=cryptocurrencyjobs.co` marker. Typically produces ~70-80 jobs/run.
  - **`hn_whoshiring`** — finds the latest *Ask HN: Who is hiring?* thread via
    the Algolia search API, fetches all top-level comments, extracts URLs
    from comment HTML, prefers known-ATS URLs over generic `Other`.
    Typically produces ~200+ jobs/run.
  - **`cryptojobs`** — RSS feed at `cryptojobs.com/jobs/feed`. Structured
    fields: `externalApplicationLink` (preferred), `workFlexibility`
    (Remote/Onsite → `remote` flag), `category`, `description`. ~36 jobs/run.
  - **`thehub`** — Nordic startup board. Scrapes `/jobs` listing for
    `/jobs/{24-hex-id}` links, fetches each detail page, picks the first
    outbound non-social anchor as the apply URL. Pagination beyond page 1 is
    JS-driven so this only sees the first ~16 jobs per crawl; the rest only
    come in as new postings appear on page 1. ~3-6 jobs/run after dedup.
  - **`workingnomads`** — JSON API at `/api/exposed_jobs/`. Each entry's
    `url` is a `/job/go/{id}/` 302 to the real apply URL — the crawler
    HEAD-follows the redirect to capture the destination, then runs it
    through `classify_or_other` so jobs land under the actual ATS company,
    not under `workingnomads.com`. ~42 jobs/run, every one tagged remote.
  - **`web3career`** — parked but kept. Apply URLs are gated behind a
    `network.bondex.app` sign-up wall; the `is_auth_wall()` filter (in
    `ats.rs`) correctly rejects them, so this crawler now contributes 0 rows
    instead of polluting the DB. Re-enable later via cookie-paste auth or a
    headless browser if needed.

### Candidates evaluated and rejected

Triaged a batch of aggregators; the following were not added, with reason:

| Site | Why skipped |
|---|---|
| `startup.jobs` | Cloudflare managed challenge (`cf-mitigated: challenge`) on every path. Needs headless browser. |
| `remotifyeurope.com` | Next.js SPA, no `__NEXT_DATA__` blob — jobs only render after JS. |
| `remoteineurope.com` | Server-rendered `/job/{slug}` exists but apply-URL discovery is hidden behind detail-page UI; revisit later. |
| `euremotejobs.com` | Front page 200 but `/jobs/` and `/feed/` return 403 — selective anti-bot. |
| `us.welcometothejungle.com` | Next.js SPA — only nav + one `/companies/` link in initial HTML. |
| `trueup.io` | 403 on Chrome UA. |
| `workinstartups.com` | 429 rate-limited from the first probe. |
| `builtin.com` | Server-rendered with `/job/{slug}/{id}` URLs but 383KB pages at scale; deferred — would need careful rate limiting. |
| `eu-startups.com` | 403 on Chrome UA. |
| `remote100k.com` | Cloudflare `challenges.cloudflare.com` script embedded; only category nav in SSR HTML. |
| `sailonchain.com` | Next.js SPA; no embedded JSON blob. |
| `laborx.com` | `/freelance-jobs/` 301→404, target is broken. |
| `globallogic.com` | 403. |
| `europeanremote.com` | Angular SPA (`ng-version`, `_ngcontent-*`); only 1 outbound apply URL reaches the static HTML — built and discarded after measuring real yield. |
| `wearedevelopers.com` | Server-rendered but only ~2 EU-remote results visible at a time and apply URLs are internal `/en/companies/{cid}/{jid}/...` — low yield for the effort. |
| `rustjobs.dev` | Vercel JS challenge (already noted in original roadmap). |

Three rough patterns to watch for: Cloudflare/anti-bot (`startup.jobs`, `trueup.io`, `eu-startups.com`, `globallogic.com`), JS-only SPAs with no SSR data
(`remotifyeurope.com`, `us.welcometothejungle.com`, `sailonchain.com`, `europeanremote.com`), and rate limiting on first contact (`workinstartups.com`,
`remote100k.com`). Any of these could be re-enabled with a headless-browser
scraper, but that's a much bigger lift than the current `reqwest`-based
pattern.
- The `Other` ATS kind catches anything not matching a known ATS — company
  careers pages, sub-aggregators (`jobs.solana.com`, `careers.smartrecruiters.com`,
  EU Greenhouse mirrors, ...). These aren't dropped — they're stored with the
  URL host as slug so you can still see them in `list jobs`.
- **ATS adapters wired for Greenhouse, Ashby, Lever.** Each sync now populates
  the plain-text description, the structured `remote` flag (Ashby's `isRemote`,
  Lever's `workplaceType`, Greenhouse's office/location names matched against
  `remote`/`anywhere`/`work from home`), and the per-job raw JSON. Other ATS
  adapters (Workable, Breezy, ...) are still pending.

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
