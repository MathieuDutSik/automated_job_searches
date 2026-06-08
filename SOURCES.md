# Sources

Where the data in `jobs.db` comes from: which aggregators `ajs` scrapes
(crawlers), which ATS APIs it pulls live jobs from (adapters), and which
candidates were evaluated and rejected. For the trait definitions and the
how-to of adding a new source, see [DESIGN.md](DESIGN.md).

## Crawlers

Each crawler discovers `(company, ats_kind, ats_slug)` tuples from one
aggregator site. The order below matches the registry in
`src/crawlers/mod.rs`.

### `cryptocurrencyjobs`

Fetches the RSS feed at `https://cryptocurrencyjobs.co/index.xml`, then for
each item fetches the detail page and extracts the apply URL via the
`?ref=cryptocurrencyjobs.co` marker. Typically produces ~70-80 jobs/run.

### `cryptojobs`

RSS feed at `https://www.cryptojobs.com/jobs/feed`. Structured fields:
`externalApplicationLink` (preferred — outbound), `workFlexibility`
(`Remote` / `Onsite` → mapped to `remote` flag), `category` (→ department),
`description`, `jobLocation`. Falls back to the cryptojobs.com URL when the
external link is empty. ~36 jobs/run.

### `hn_whoshiring`

Finds the latest *Ask HN: Who is hiring?* thread via the Algolia search
API, fetches all top-level comments, extracts URLs from comment HTML,
prefers known-ATS URLs over generic `Other`. Typically produces ~200+
jobs/run.

### `thehub`

Nordic startup board. Scrapes `https://thehub.io/jobs` for `/jobs/{24-hex-id}`
links, fetches each detail page, picks the first outbound non-social anchor
as the apply URL. Pagination beyond page 1 is JS-driven, so this only sees
the first ~16 jobs per crawl; the rest only come in as new postings rotate
onto page 1. ~3-6 jobs/run after dedup.

### `web3career`

Parked but kept registered. Apply URLs are gated behind a `network.bondex.app`
sign-up wall; the `is_auth_wall()` filter in `ats.rs` rejects them, so this
crawler contributes 0 rows instead of polluting the DB. Re-enable later via
cookie-paste auth or a headless browser.

### `workingnomads`

JSON API at `https://www.workingnomads.com/api/exposed_jobs/`. Each entry's
`url` is a `/job/go/{id}/` 302 to the real apply URL — the crawler
HEAD-follows the redirect to capture the destination, then runs that URL
through `classify_or_other` so jobs land under the actual ATS company, not
under `workingnomads.com`. Description, location, tags, and remote flag
(inferred from location / tags containing `remote` / `anywhere` /
`worldwide`) are all preserved. ~42 jobs/run, nearly all remote.

## ATS adapters

Wired for seven ATSes. Each sync populates the plain-text description
(when the ATS exposes one in the listing API), the structured `remote`
flag, and the per-job raw JSON.

- **Ashby** — `isRemote`, `descriptionPlain` (cheap — already in the
  posting API response).
- **Lever** — `descriptionPlain`, `workplaceType` (`remote` / `on-site` /
  `hybrid` / `unspecified`).
- **Greenhouse** — uses `?content=true`. Description is HTML-entity-encoded;
  the adapter decodes entities, strips tags via `scraper`, collapses
  whitespace. Remote detection is heuristic: any office or location name
  matching `remote` / `anywhere` / `work from home` flips the flag.
- **SmartRecruiters** — `https://api.smartrecruiters.com/v1/companies/{Slug}/postings`,
  paginated at 100/page until `totalFound` is reached. Apply URL is
  reconstructed as `https://jobs.smartrecruiters.com/{Slug}/{id}`. The
  `remote` flag comes from `location.remote`. **No description in the
  listing API** — would need a per-posting fetch
  (`/postings/{id}`); deferred. Bosch Group is the canonical big-tenant
  smoke test (~4500 postings).
- **BambooHR** — `https://{slug}.bamboohr.com/careers/list`. Single-shot
  (no pagination — endpoint always returns the full set). `isRemote`
  field present in the response; falls back to `locationType == "2"` or
  the location name containing `remote`. **No description in the listing
  API.**
- **Recruitee** — `https://{slug}.recruitee.com/api/offers/`. Single-shot.
  Concatenates `description` + `requirements` (both HTML), strips tags,
  for the FTS-indexed description. Carries `remote` directly.
- **Workday** — POST `https://{tenant}.{wdN}.myworkdayjobs.com/wday/cxs/{tenant}/{site}/jobs`
  with `{"limit":20,"offset":N,"appliedFacets":{},"searchText":""}`,
  paginated. The composite slug `tenant/wdN/site` is split inside the
  adapter (see [DESIGN.md](DESIGN.md) for why Workday needs a composite
  slug). One quirk: Workday returns `total` only on page 1; subsequent
  pages report `total: 0`, so the adapter pins the figure from page 1
  and uses it as the upper bound. `remote` is heuristic from
  `locationsText` (`Remote` / `Anywhere`). **No description in the
  listing endpoint** — Workday has a separate per-job POST that the
  adapter doesn't call yet.

The catch-all `AtsKind::Other` covers anything not matching a known ATS —
company careers pages, sub-aggregators (`jobs.solana.com`, EU Greenhouse
mirrors, ...). These aren't dropped; they're stored with the URL host as
slug so they still appear in `list jobs`.

Pending adapters (see [DESIGN.md](DESIGN.md) roadmap): Workable, Breezy,
Personio, JazzHR, Teamtailor, Pinpoint, Comeet.

## Search-engine discovery (`ajs discover`)

`discover` complements `crawl` by running targeted `site:` / phrase
queries through a web-search engine, classifying each returned URL, and
upserting matching slugs as new `companies` rows that `sync` will then
hit. Query templates per ATS live in `src/discover.rs::PLANS`.

### Available engines (`--engine <name>`)

| Engine | Default | Env vars | Free tier | `count` cap | `site:` works on free? |
|---|---|---|---|---|---|
| `brave` | ✓ | `BRAVE_API_KEY` | 2k/mo, 1 req/s | 20 | ✓ (patchy on subdomains) |
| `google` |   | `GOOGLE_API_KEY`, `GOOGLE_CSE_ID` | 100/day | 10 | ✓ |
| `serper` |   | `SERPER_API_KEY` | 2.5k credits | — | **✗ — free tier rejects `site:` with HTTP 400** |
| `tavily` |   | `TAVILY_API_KEY` | dev key, ~1k/mo | 20 | ✓ |
| `exa` |   | `EXA_SECRET_KEY` | 1k/mo | 10 | ✓ (auto-converted to `includeDomains`) |
| `firecrawl` |   | `FIRECRAWL_DEV_API_KEY` | 500 credits | 20 | ✓ (Google-backed) |

### Brave Search backend

- Default engine — selected when `--engine` is omitted.
- 1.1s politeness sleep between queries to stay under the free-tier rate
  limit.
- `count` is hard-capped at 20 per Brave's API.

### Tavily backend

- Best free-tier behaviour of the cheap options: honors `site:` queries
  and returns real subdomain URLs (verified live for `site:jobs.ashbyhq.com`
  returning Ashby tenant URLs).
- POST `https://api.tavily.com/search` with `Authorization: Bearer …`.
- Set `search_depth: "basic"` — `"advanced"` costs more credits per query
  with marginal benefit for our URL-list use case.

### EXA backend

- POST `https://api.exa.ai/search` with `x-api-key` header.
- EXA doesn't honor `site:` as a query-string operator; the canonical way
  to filter by host is the `includeDomains` body field. The adapter
  automatically extracts any `site:foo.com` token from the query and moves
  it into `includeDomains`, so the same PLANS list works unchanged. Tests
  in `src/search/exa.rs` cover this translation.
- When the query is *only* `site:foo.com` with no keywords, the adapter
  substitutes `"remote jobs"` (EXA rejects empty queries).

### Firecrawl backend

- POST `https://api.firecrawl.dev/v1/search` with `Authorization: Bearer …`.
- Firecrawl's search is Google-backed, so it returns the same URL set as
  Serper would on paid tier. Primary value here is the cheap free tier
  (500 credits) as a Google-quality alternative.
- The product is primarily a JS-rendering scraper; the JS-SPA aggregators
  rejected in the table below could become viable via Firecrawl's
  `/v1/scrape` endpoint later, behind a separate crawler module.

### Serper backend (limited usefulness on free tier)

- Wired up but **the free Serper plan rejects every query containing
  `site:`** with HTTP 400 "Query pattern not allowed for free accounts".
  Every plan in `discover.rs::PLANS` uses `site:`, so `--engine serper`
  produces zero results on the free tier today.
- Paid tier ($50/mo for ~50k queries) unlocks operators and would make
  this engine drop-in equivalent to Google CSE. Kept registered so the
  upgrade path is trivial if needed.

### Google CSE backend

Highest-quality recall of the three but requires one-time off-the-terminal
setup:

1. **Programmable Search Engine** — go to
   <https://programmablesearchengine.google.com/>, "Add", set any name,
   under "What to search?" pick **"Search the entire web"** (critical — by
   default it only searches sites you list). Save and copy the **Search
   engine ID** (looks like `017576662512468239146:omuauf_lfve`).

2. **Custom Search API key** — Google Cloud Console → create or pick a
   project → APIs & Services → enable **"Custom Search API"** → Credentials
   → "Create credentials" → API key. Optionally restrict the key to the
   Custom Search API.

3. **Env vars** — `export GOOGLE_API_KEY=...` and `export GOOGLE_CSE_ID=...`.

`num` (results per query) is capped at 10 by the API; deeper pagination
needs the `start` query parameter and a loop — not wired today.

### Per-ATS observations on Brave

| ATS | Query strategy | Brave yield (sample) |
|---|---|---|
| Greenhouse | `site:boards.greenhouse.io "Remote"` (+ `job-boards.greenhouse.io`) | Works well — TLD-distinguished |
| Ashby | `site:jobs.ashbyhq.com "Remote"` | Works well |
| Lever | `site:jobs.lever.co "Remote"` | Works well |
| SmartRecruiters | `site:jobs.smartrecruiters.com "Remote"` | Works well |
| Recruitee | `site:recruitee.com "Remote"` | **Excellent** — Brave returns 20 per-tenant subdomains directly; ~15 new companies per query |
| Workday | `site:myworkdayjobs.com "Remote"` | **Excellent** — same pattern; big-name tenants land immediately (Cisco, Novartis, U-Haul, ...) |
| BambooHR | `site:bamboohr.com {keyword} remote` × N keywords | **Poor** — Brave's index is dominated by the marketing `www.bamboohr.com` site; need multiple keyword-rich queries to fish out per-tenant boards. ~5 new companies per `discover bamboohr`. |

### Brave-specific quirks

- **`inurl:` is non-functional** — silently returns 0 results, dropped
  from all plans.
- **`site:` subdomain inclusion varies by root** — `recruitee.com` and
  `myworkdayjobs.com` return subdomain URLs by default; `bamboohr.com`
  collapses to `www.`.
- **Pre-classifier guards in `ats.rs`** reject obvious non-tenant
  subdomains (`www`, `developers`, `api`, `docs`, ...) and URLs that
  don't carry an expected careers/jobs path component. Without these,
  marketing pages would upsert as bogus "companies".

### When to override the default engine

- **Brave** (default) is fine for the four TLD-distinguished ATSes
  (Greenhouse, Ashby, Lever, SmartRecruiters) and for Recruitee / Workday
  — those return clean subdomain results out of the box.
- **`--engine google`** when Brave's index feels stale (it keeps
  long-closed jobs in `site:` results) or when discovering a new ATS for
  the first time.
- **`--engine tavily`** as a free-tier alternative whose backing index
  differs from Brave's; running both then deduping covers more of the
  long tail.
- **`--engine exa`** when neural/semantic ranking might surface things
  keyword search misses.
- **`--engine firecrawl`** as a Google-quality cross-check on a small
  free credit budget.
- **`--engine serper`** only on the paid tier — the free tier rejects
  every `site:`-bearing query (HTTP 400).

Engines are independent — running `discover ashby --engine brave` then
`discover ashby --engine tavily` will surface a wider company set than
either alone, since each `upsert_company` is idempotent on
`(ats_kind, ats_slug)`.

## Candidates evaluated and rejected

Aggregators that were triaged and **not** added, with the reason:

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
| `rustjobs.dev` | Vercel JS challenge. |

Three rough patterns to watch for:

- **Cloudflare / anti-bot** (`startup.jobs`, `trueup.io`, `eu-startups.com`,
  `globallogic.com`) — would need a headless browser to bypass.
- **JS-only SPAs with no SSR data** (`remotifyeurope.com`,
  `us.welcometothejungle.com`, `sailonchain.com`, `europeanremote.com`) —
  same; the SSR HTML doesn't carry the listings.
- **Rate limiting on first contact** (`workinstartups.com`,
  `remote100k.com`) — may work if you back off, but the SSR HTML on
  `remote100k.com` is already mostly nav, so the upside is small.

## Roadmap

1. **More crawlers** — `weworkremotely.com`, `remote.co`, `jobspresso.co`,
   getro VC-portfolio boards.
2. **Auth-walled sources** — cookie-paste support for `web3.career`;
   revisit `rustjobs.dev` (Vercel JS challenge) with a headless browser if
   it ever looks worth it.
