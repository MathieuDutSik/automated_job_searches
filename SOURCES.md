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

Wired for Greenhouse, Ashby, and Lever. Each sync populates the plain-text
description, the structured `remote` flag, and the per-job raw JSON:

- **Ashby** — `isRemote`, `descriptionPlain` (cheap — already in the
  posting API response).
- **Lever** — `descriptionPlain`, `workplaceType` (`remote` / `on-site` /
  `hybrid` / `unspecified`).
- **Greenhouse** — uses `?content=true`. Description is HTML-entity-encoded;
  the adapter decodes entities, strips tags via `scraper`, collapses
  whitespace. Remote detection is heuristic: any office or location name
  matching `remote` / `anywhere` / `work from home` flips the flag.

The catch-all `AtsKind::Other` covers anything not matching a known ATS —
company careers pages, sub-aggregators (`jobs.solana.com`,
`careers.smartrecruiters.com`, EU Greenhouse mirrors, ...). These aren't
dropped; they're stored with the URL host as slug so they still appear in
`list jobs`.

Pending adapters (see [DESIGN.md](DESIGN.md) roadmap): Workable, Breezy,
Smartrecruiters, Recruitee, Personio, JazzHR, Teamtailor, BambooHR,
Pinpoint, Workday.

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
