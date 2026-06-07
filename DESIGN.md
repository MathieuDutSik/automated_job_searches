# Design

Internals of `ajs`: the schema, the module layout, and the two extension
traits (`Crawler` and `AtsAdapter`). For user-facing CLI instructions see
[README.md](README.md); for the list of supported sources see
[SOURCES.md](SOURCES.md).

## Two-phase model

`ajs` separates **discovery** from **sync**:

1. **Crawlers** scrape aggregator sites to find which company is hosted on
   which ATS — output is `(company, ats_kind, ats_slug)` tuples (plus a
   sample of jobs).
2. **Adapters** take a known slug and pull the full live job list from the
   ATS's public JSON API.

The typical workflow is `crawl all` once to populate `companies`, then
`sync all` whenever you want fresh job lists. Both phases are idempotent;
re-runs update `last_seen` and mark disappeared jobs `closed_at`.

## What it stores

A SQLite file (default `<repo>/jobs.db`) with:

- `companies` — one row per `(ats_kind, ats_slug)`. Display name +
  first/last seen timestamps.
- `company_discoveries` — append-only log of which crawler saw which
  company, when, and on what page.
- `jobs` — one row per `(ats_kind, external_id)`. Title, location, apply
  URL, description (plain text), `remote` flag, raw API blob, plus
  `first_seen` / `last_seen` / `closed_at` for ATS-side lifecycle, and
  `status` (`new` | `applied` | `dismissed`) + `status_changed_at` /
  `status_note` for the user's pipeline state. `closed_at` is what the ATS
  says; `status` is what the user says — they're independent.
- `jobs_fts` — FTS5 virtual table mirroring `title` / `location` /
  `department` / `description`, kept in sync by triggers. Powers
  `list jobs --match <query>`. Tokenizer is `unicode61 tokenchars '+#.'` —
  words are split on whitespace and punctuation **except** `+`, `#`, `.`,
  so `c++` / `c#` / `.net` index as single tokens (quote them as phrases
  when querying), while `rust` matches only the word `rust` and not
  `trusted` / `trust`. Separators include space, `/`, `-`, `,`, `(`, `)`,
  etc. — anything not a word character or in `tokenchars`.
- `crawl_runs` — one row per crawler invocation, with counts and errors.
  Use it to spot which sources are healthy.
- `meta` — internal key/value (currently tracks the FTS5 build version,
  so a schema bump triggers one automatic rebuild on next open).

The schema is created on first open; re-running is idempotent.

### Migration strategy

`db::SCHEMA_BASE` runs CREATE TABLE statements (all `IF NOT EXISTS`).
`ensure_column` then ADD COLUMNs anything new on an existing DB by
inspecting `PRAGMA table_info`. Finally `db::SCHEMA_DERIVED` runs the
indexes, FTS virtual table, and triggers — which may depend on the
newly-added columns. The FTS table is dropped + recreated when
`meta.fts_built` doesn't match the in-code `FTS_VERSION`; bump
`FTS_VERSION` whenever the FTS5 column set or tokenizer changes.

## Layout

```
src/
  main.rs            # clap entrypoint: crawl | sync | list | status | mark
  db.rs              # schema, migrations, upsert helpers, list_jobs_filtered
  http.rs            # shared reqwest client (UA, timeout, gzip/brotli)
  ats.rs             # classify_apply_url() — recognizes 14 ATS URL patterns
  crawlers/          # discovery: find (company, ats_kind, ats_slug)
    mod.rs           # trait Crawler + registry
    ...              # one module per crawler — see SOURCES.md
  adapters/          # sync: for a known slug, pull all live jobs from ATS API
    mod.rs           # trait AtsAdapter + registry + sync_all_for_kind()
    ...              # one module per ATS — see SOURCES.md
```

## Crawler trait

```rust
#[async_trait(?Send)]
pub trait Crawler {
    fn name(&self) -> &'static str;
    async fn run(&self, db: &Db) -> Result<CrawlReport>;
}
```

Each `run` is expected to:

1. Fetch a listing (RSS / JSON / HTML).
2. For each item, derive an apply URL (often from a detail page).
3. Pass that URL through `ats::classify_or_other` to get
   `(kind, slug, external_id)`.
4. `db.upsert_company(...)` then `db.upsert_job(JobUpsert { ... })`.
5. Return a `CrawlReport` with counts; `main.rs` writes it into
   `crawl_runs`.

If the URL doesn't match a known ATS, `classify_or_other` returns
`AtsKind::Other` with the host as slug — those rows aren't dropped, they
still show up in `list jobs`.

## Adapter trait

```rust
#[async_trait(?Send)]
pub trait AtsAdapter {
    fn kind(&self) -> AtsKind;
    async fn fetch_jobs(&self, slug: &str) -> Result<Vec<AdapterJob>>;
}
```

Each `AdapterJob` carries:

- `external_id` / `title` / `location` / `department` / `apply_url`
- `description` — plain text; HTML-decoded for adapters that return
  HTML-only descriptions (Greenhouse), passed through for those that
  expose a `descriptionPlain` field (Ashby, Lever)
- `remote` — populated from structured ATS fields when available
  (Ashby's `isRemote`, Lever's `workplaceType`); heuristic for Greenhouse
  (office / location name matched against `remote` / `anywhere` /
  `work from home`)
- `posted_at` / `raw_json`

`sync_all_for_kind()` does the rest: iterating every known slug for that
`AtsKind`, honoring a politeness delay, calling `fetch_jobs`, upserting
each row, recording 404s, and sweeping unseen jobs to `closed_at`.

`raw_json` is preserved per-job: adapters fetch the response as
`serde_json::Value`, deserialize a typed view from each entry, and stringify
the same entry back to `raw_json`. Unknown fields survive.

## Adding a new crawler

1. Create `src/crawlers/<name>.rs` with a `pub struct YourName;` and
   `impl Crawler for YourName`.
2. Register the module in `src/crawlers/mod.rs`:
   - `pub mod <name>;`
   - add `Box::new(<name>::YourName)` to the `all()` vector.
3. Run `cargo test` — the registry is exercised by the integration code.

## Adding a new ATS adapter

1. Create `src/adapters/<kind>.rs` with `impl AtsAdapter for YourKind`,
   returning a `Vec<AdapterJob>` from a single slug.
2. Add the variant to `AtsKind` in `ats.rs` if it's a new ATS; otherwise
   add the URL pattern to `classify_apply_url`.
3. Register the module in `src/adapters/mod.rs`:
   - `pub mod <kind>;`
   - add `Box::new(<kind>::YourKind)` to the `all()` vector.
4. The shared `sync_all_for_kind()` handles iteration, the politeness
   delay, upserting, 404 handling, and the close-unseen-jobs sweep.

## Composite slugs

`companies.ats_slug` is a single TEXT column. For ATSes whose API needs
multiple identifiers, the slug encodes them with `/` separators:

- **Workday**: `tenant/wd{N}/site` — e.g. `nvidia/wd5/NVIDIAExternalCareerSite`.
  - `tenant` and `wd{N}` come from the host (`nvidia.wd5.myworkdayjobs.com`).
  - `site` is the first path segment that isn't a language code (`en-US`,
    `fr_FR`, ...), grabbed by `is_lang_code()` in `ats.rs`.
  - The adapter splits the slug back inside `fetch_jobs`.

If a Workday URL lacks a site segment (bare tenant root, e.g.
`https://nvidia.wd5.myworkdayjobs.com/`), `classify_apply_url` returns
`None` — we can't sync it without the site name. Earlier short slugs (just
`tenant`) from before this scheme will fail at sync time with a clear error
and can be removed from the DB.

All other ATSes use a flat single-token slug.

## Roadmap

1. **More ATS adapters** — Workable, Breezy, Personio, JazzHR, Teamtailor,
   Pinpoint, Comeet. Same pattern as the existing seven; each is ~50-80
   lines.
2. **Descriptions for SmartRecruiters / BambooHR / Workday** — these three
   only expose summary data in the listing API; a separate per-posting
   fetch is required (and at SmartRecruiters/Workday scale, that's
   thousands of extra requests per sync). Worth doing behind a `--full`
   flag if rich text matters.
3. **`discover` command** — Google Custom Search Engine queries like
   `site:boards.greenhouse.io "engineer" "remote"` to find new ATS slugs
   beyond what crawlers surface.
