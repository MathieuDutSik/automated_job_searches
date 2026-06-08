# automated_job_searches

CLI tool that crawls job aggregators, discovers companies hosted on known
ATS platforms (Greenhouse, Ashby, Lever, Workable, ...), and stores
everything in a local SQLite file for inspection and re-crawling.

See also: [DESIGN.md](DESIGN.md) for the schema and module layout,
[SOURCES.md](SOURCES.md) for the list of supported sources and rejected
candidates.

## Build

```sh
cargo build --release
```

The binary is `target/release/ajs`. For dev use, `cargo run --` works the
same.

## Usage

There are two phases. **Crawlers** discover which company is hosted on
which ATS. **Adapters** then sync the full live job list for every known
company from the ATS's public JSON API.

```sh
# Phase 1a: discover companies via aggregator crawlers
ajs crawl <name>                  # one crawler (`ajs crawl --help` for names)
ajs crawl all                     # every registered crawler

# Phase 1b: discover companies via web-search queries
ajs discover <ats>                          # one ATS, default engine (brave)
ajs discover all                            # every ATS plan, default engine
ajs discover ashby --engine google          # use Google CSE for this run
ajs discover ashby --engine tavily          # cross-check with a different index
#
# Engines and the env vars they need (see SOURCES.md for details):
#   brave     → BRAVE_API_KEY                                (default; 2k q/mo free)
#   google    → GOOGLE_API_KEY + GOOGLE_CSE_ID               (100 q/day free, $5/1k after)
#   tavily    → TAVILY_API_KEY                               (dev tier ~1k q/mo)
#   exa       → EXA_SECRET_KEY                               (1k q/mo free)
#   firecrawl → FIRECRAWL_DEV_API_KEY                        (500 credits free)
#   serper    → SERPER_API_KEY                               (paid only — free rejects site:)

# Phase 2: refresh full job lists from the ATS JSON APIs
ajs sync <ats>                    # one ATS (iterates every company of that kind)
ajs sync all                      # every registered adapter

# Inspect the data
ajs list companies --limit 50           # flat list of companies
ajs list jobs                           # flat list of open jobs (unlimited by default)
ajs list jobs --limit 50                # cap to 50 rows
ajs list jobs --start 1000 --limit 50   # paginate: rows 1000..1049
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

The typical workflow is `crawl all` (and/or `discover all`) once to
populate `companies`, then `sync all` whenever you want fresh job lists
(idempotent — re-runs update `last_seen` and mark disappeared jobs
`closed_at`). `discover` is targeted at specific ATSes (you pick which
to query) while `crawl` walks general aggregator sites.

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
```

### Pointing at a different DB file

```sh
ajs --db /path/to/jobs.db crawl all
```

The default database path is `<repo>/jobs.db` — the absolute path to this
checkout is baked into the binary at build time, so `ajs` writes to the
same file no matter what directory you run it from. Rebuild after moving
the checkout, or pass `--db` explicitly.

### Logging

Logging is via `RUST_LOG`. Default is `info`. For more detail:

```sh
RUST_LOG=debug ajs crawl <name>
```
