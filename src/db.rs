use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::ats::AtsKind;

// Base tables (idempotent via IF NOT EXISTS). Anything that depends on a
// column added after the initial release (e.g. `jobs.status`) goes into
// SCHEMA_DERIVED, applied AFTER ensure_column migrations have run.
const SCHEMA_BASE: &str = r#"
CREATE TABLE IF NOT EXISTS companies (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    ats_kind     TEXT NOT NULL,
    ats_slug     TEXT NOT NULL,
    website      TEXT,
    first_seen   TEXT NOT NULL,
    last_seen    TEXT NOT NULL,
    UNIQUE(ats_kind, ats_slug)
);

CREATE TABLE IF NOT EXISTS company_discoveries (
    id             INTEGER PRIMARY KEY,
    company_id     INTEGER NOT NULL REFERENCES companies(id),
    discovered_via TEXT NOT NULL,
    discovered_at  TEXT NOT NULL,
    source_url     TEXT
);

CREATE TABLE IF NOT EXISTS jobs (
    id                INTEGER PRIMARY KEY,
    company_id        INTEGER NOT NULL REFERENCES companies(id),
    ats_kind          TEXT NOT NULL,
    external_id       TEXT NOT NULL,
    title             TEXT NOT NULL,
    location          TEXT,
    remote            INTEGER,
    department        TEXT,
    apply_url         TEXT NOT NULL,
    description       TEXT,
    posted_at         TEXT,
    first_seen        TEXT NOT NULL,
    last_seen         TEXT NOT NULL,
    closed_at         TEXT,
    raw_json          TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'new',
    status_changed_at TEXT,
    status_note       TEXT,
    UNIQUE(ats_kind, external_id)
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS crawl_runs (
    id           INTEGER PRIMARY KEY,
    source       TEXT NOT NULL,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    ok           INTEGER,
    http_status  INTEGER,
    items_seen   INTEGER,
    items_new    INTEGER,
    error        TEXT
);
"#;

const SCHEMA_DERIVED: &str = r#"
CREATE INDEX IF NOT EXISTS idx_jobs_open    ON jobs(closed_at) WHERE closed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_jobs_company ON jobs(company_id);
CREATE INDEX IF NOT EXISTS idx_jobs_remote  ON jobs(remote) WHERE remote = 1 AND closed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_jobs_status  ON jobs(status) WHERE status != 'new';

CREATE VIRTUAL TABLE IF NOT EXISTS jobs_fts USING fts5(
    title, location, department, description,
    content='jobs', content_rowid='id',
    tokenize = "unicode61 tokenchars '+#.'"
);

CREATE TRIGGER IF NOT EXISTS jobs_ai AFTER INSERT ON jobs BEGIN
  INSERT INTO jobs_fts(rowid, title, location, department, description)
  VALUES (new.id, new.title, new.location, new.department, new.description);
END;
CREATE TRIGGER IF NOT EXISTS jobs_ad AFTER DELETE ON jobs BEGIN
  INSERT INTO jobs_fts(jobs_fts, rowid, title, location, department, description)
  VALUES ('delete', old.id, old.title, old.location, old.department, old.description);
END;
CREATE TRIGGER IF NOT EXISTS jobs_au AFTER UPDATE ON jobs BEGIN
  INSERT INTO jobs_fts(jobs_fts, rowid, title, location, department, description)
  VALUES ('delete', old.id, old.title, old.location, old.department, old.description);
  INSERT INTO jobs_fts(rowid, title, location, department, description)
  VALUES (new.id, new.title, new.location, new.department, new.description);
END;
"#;

pub struct Db {
    conn: Connection,
}

/// One row returned by `list_jobs_filtered` for display in `ajs list jobs`.
/// Mirrors the SELECT list of that query.
#[derive(Debug, Clone)]
pub struct JobListing {
    pub id: i64,
    pub company: String,
    pub title: String,
    pub location: String,
    pub apply_url: String,
    pub remote: Option<bool>,
    pub status: String,
}

/// Compact job tuple shown under a company in `ajs list by-company`.
#[derive(Debug, Clone)]
pub struct JobSummary {
    pub title: String,
    pub location: String,
    pub apply_url: String,
}

/// One row returned by `list_by_company` — a company plus its open jobs.
#[derive(Debug, Clone)]
pub struct CompanyWithJobs {
    pub name: String,
    pub kind: String,
    pub slug: String,
    pub jobs: Vec<JobSummary>,
}

/// One row returned by `find_companies_by_slug`. Used by `ajs mark`'s
/// `company:` selector to resolve a slug → company_id; the kind/slug
/// fields are reported back in messages so the caller can re-run with a
/// kind-qualified selector to disambiguate when multiple matches exist.
#[derive(Debug, Clone)]
pub struct CompanyMatch {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub slug: String,
}

pub struct JobUpsert<'a> {
    pub company_id: i64,
    pub kind: AtsKind,
    pub external_id: &'a str,
    pub title: &'a str,
    pub location: Option<&'a str>,
    pub department: Option<&'a str>,
    pub apply_url: &'a str,
    pub description: Option<&'a str>,
    pub remote: Option<bool>,
    pub posted_at: Option<&'a str>,
    pub raw_json: &'a str,
}

impl Db {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(&path)
            .with_context(|| format!("opening sqlite at {}", path.as_ref().display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_BASE)?;
        // CREATE TABLE IF NOT EXISTS doesn't help when columns are added later.
        // Run idempotent ADD COLUMN for any column missing on an existing DB,
        // BEFORE the derived schema (indexes/FTS/triggers may reference them).
        ensure_column(&conn, "jobs", "status", "TEXT NOT NULL DEFAULT 'new'")?;
        ensure_column(&conn, "jobs", "status_changed_at", "TEXT")?;
        ensure_column(&conn, "jobs", "status_note", "TEXT")?;

        // External-content FTS5 tables report the linked table's row count for
        // COUNT(*) — useless as a "is the index built?" signal. Track with a
        // meta key instead; bump FTS_VERSION whenever the virtual-table
        // definition (columns, tokenizer, ...) changes meaningfully, and the
        // next open drops + recreates the FTS table to pick up the change.
        // v1 → v2: trigram → unicode61 with tokenchars '+#.' (word-bounded
        // matching so `rust` no longer matches `trusted`, while `c++` etc.
        // still index as single tokens).
        const FTS_VERSION: &str = "2";
        let built: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'fts_built'", [], |r| {
                r.get(0)
            })
            .optional()?;
        let fts_stale = built.as_deref() != Some(FTS_VERSION);
        if fts_stale {
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS jobs_ai;
                 DROP TRIGGER IF EXISTS jobs_ad;
                 DROP TRIGGER IF EXISTS jobs_au;
                 DROP TABLE IF EXISTS jobs_fts;",
            )?;
        }
        conn.execute_batch(SCHEMA_DERIVED)?;
        if fts_stale {
            conn.execute("INSERT INTO jobs_fts(jobs_fts) VALUES('rebuild')", [])?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES('fts_built', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![FTS_VERSION],
            )?;
        }
        Ok(Self { conn })
    }

    /// `name_hint` is the authoritative display name when we know it. Pass
    /// `None` if we only have a slug — the slug will be used on first insert
    /// and existing names won't be clobbered.
    pub fn upsert_company(
        &self,
        name_hint: Option<&str>,
        kind: AtsKind,
        slug: &str,
        discovered_via: &str,
        source_url: Option<&str>,
    ) -> Result<(i64, bool)> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM companies WHERE ats_kind = ? AND ats_slug = ?",
                params![kind.as_str(), slug],
                |r| r.get(0),
            )
            .optional()?;
        let (id, is_new) = match existing {
            Some(id) => {
                match name_hint {
                    Some(n) => tx.execute(
                        "UPDATE companies SET last_seen = ?, name = ? WHERE id = ?",
                        params![now, n, id],
                    )?,
                    None => tx.execute(
                        "UPDATE companies SET last_seen = ? WHERE id = ?",
                        params![now, id],
                    )?,
                };
                (id, false)
            }
            None => {
                let name = name_hint.unwrap_or(slug);
                tx.execute(
                    "INSERT INTO companies (name, ats_kind, ats_slug, first_seen, last_seen) VALUES (?, ?, ?, ?, ?)",
                    params![name, kind.as_str(), slug, now, now],
                )?;
                (tx.last_insert_rowid(), true)
            }
        };
        tx.execute(
            "INSERT INTO company_discoveries (company_id, discovered_via, discovered_at, source_url) VALUES (?, ?, ?, ?)",
            params![id, discovered_via, now, source_url],
        )?;
        tx.commit()?;
        Ok((id, is_new))
    }

    pub fn upsert_job(&self, j: JobUpsert<'_>) -> Result<(i64, bool)> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM jobs WHERE ats_kind = ? AND external_id = ?",
                params![j.kind.as_str(), j.external_id],
                |r| r.get(0),
            )
            .optional()?;
        let remote_i = j.remote.map(|b| b as i64);
        let (id, is_new) = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE jobs SET last_seen = ?, title = ?, location = ?, department = ?, apply_url = ?, description = COALESCE(?, description), remote = COALESCE(?, remote), posted_at = ?, raw_json = ?, closed_at = NULL WHERE id = ?",
                    params![now, j.title, j.location, j.department, j.apply_url, j.description, remote_i, j.posted_at, j.raw_json, id],
                )?;
                (id, false)
            }
            None => {
                tx.execute(
                    "INSERT INTO jobs (company_id, ats_kind, external_id, title, location, department, apply_url, description, remote, posted_at, first_seen, last_seen, raw_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![j.company_id, j.kind.as_str(), j.external_id, j.title, j.location, j.department, j.apply_url, j.description, remote_i, j.posted_at, now, now, j.raw_json],
                )?;
                (tx.last_insert_rowid(), true)
            }
        };
        tx.commit()?;
        Ok((id, is_new))
    }

    /// Mark all open jobs for a given company that were last seen before
    /// `sync_started` as closed. Used by ATS-adapter syncs to reflect job
    /// disappearance: anything we didn't see in the latest fetch is gone.
    /// Returns the number of jobs that were just closed.
    pub fn mark_unseen_jobs_closed(
        &self,
        company_id: i64,
        kind: AtsKind,
        sync_started: &str,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE jobs SET closed_at = ? WHERE company_id = ? AND ats_kind = ? AND closed_at IS NULL AND last_seen < ?",
            params![now, company_id, kind.as_str(), sync_started],
        )?;
        Ok(n)
    }

    pub fn list_slugs_for_kind(&self, kind: AtsKind) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, ats_slug FROM companies WHERE ats_kind = ? ORDER BY ats_slug",
        )?;
        let rows = stmt
            .query_map(params![kind.as_str()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_by_company(&self, limit_companies: usize) -> Result<Vec<CompanyWithJobs>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, ats_kind, ats_slug FROM companies
             WHERE id IN (SELECT DISTINCT company_id FROM jobs WHERE closed_at IS NULL)
             ORDER BY name COLLATE NOCASE
             LIMIT ?",
        )?;
        struct CompanyRef {
            id: i64,
            name: String,
            kind: String,
            slug: String,
        }
        let companies: Vec<CompanyRef> = stmt
            .query_map(params![limit_companies as i64], |r| {
                Ok(CompanyRef {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    slug: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(companies.len());
        let mut jobs_stmt = self.conn.prepare(
            "SELECT title, COALESCE(location, ''), apply_url
               FROM jobs WHERE company_id = ? AND closed_at IS NULL
               ORDER BY title COLLATE NOCASE",
        )?;
        for c in companies {
            let jobs: Vec<JobSummary> = jobs_stmt
                .query_map(params![c.id], |r| {
                    Ok(JobSummary {
                        title: r.get(0)?,
                        location: r.get(1)?,
                        apply_url: r.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            out.push(CompanyWithJobs {
                name: c.name,
                kind: c.kind,
                slug: c.slug,
                jobs,
            });
        }
        Ok(out)
    }

    pub fn start_run(&self, source: &str) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO crawl_runs (source, started_at) VALUES (?, ?)",
            params![source, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(
        &self,
        run_id: i64,
        ok: bool,
        http_status: Option<u16>,
        seen: u64,
        new: u64,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE crawl_runs SET finished_at = ?, ok = ?, http_status = ?, items_seen = ?, items_new = ?, error = ? WHERE id = ?",
            params![now, ok as i64, http_status, seen as i64, new as i64, error, run_id],
        )?;
        Ok(())
    }

    pub fn list_companies(&self, limit: usize) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ats_kind, ats_slug FROM companies ORDER BY last_seen DESC LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List open jobs, optionally filtered by remote / FTS5 match / status.
    /// `limit = None` means unlimited (passed to SQLite as `LIMIT -1`).
    /// `start` is an OFFSET applied before the limit.
    pub fn list_jobs_filtered(
        &self,
        limit: Option<usize>,
        start: usize,
        remote_only: bool,
        match_query: Option<&str>,
        status: StatusFilter,
    ) -> Result<Vec<JobListing>> {
        let mut sql = String::from(
            "SELECT j.id, c.name, j.title, COALESCE(j.location, ''), j.apply_url, j.remote, j.status
               FROM jobs j JOIN companies c ON c.id = j.company_id",
        );
        if match_query.is_some() {
            sql.push_str(" JOIN jobs_fts f ON f.rowid = j.id");
        }
        sql.push_str(" WHERE j.closed_at IS NULL");
        if remote_only {
            sql.push_str(" AND j.remote = 1");
        }
        match status {
            StatusFilter::HideDismissed => sql.push_str(" AND j.status != 'dismissed'"),
            StatusFilter::All => {}
            StatusFilter::AppliedOnly => sql.push_str(" AND j.status = 'applied'"),
        }
        if match_query.is_some() {
            sql.push_str(" AND jobs_fts MATCH ?");
        }
        sql.push_str(if match_query.is_some() {
            " ORDER BY rank LIMIT ? OFFSET ?"
        } else {
            " ORDER BY j.last_seen DESC LIMIT ? OFFSET ?"
        });
        let limit_param: i64 = limit.map(|n| n as i64).unwrap_or(-1);
        let offset_param: i64 = start as i64;
        let mut stmt = self.conn.prepare(&sql)?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<JobListing> {
            Ok(JobListing {
                id: r.get(0)?,
                company: r.get(1)?,
                title: r.get(2)?,
                location: r.get(3)?,
                apply_url: r.get(4)?,
                remote: r.get::<_, Option<i64>>(5)?.map(|i| i != 0),
                status: r.get(6)?,
            })
        };
        let rows: Vec<_> = if let Some(q) = match_query {
            stmt.query_map(params![q, limit_param, offset_param], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![limit_param, offset_param], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    /// Update the per-user status on a single job row. Status must be one of
    /// `new` / `applied` / `dismissed`. Returns Err if the id doesn't exist.
    pub fn set_status(&self, id: i64, status: &str, note: Option<&str>) -> Result<()> {
        if !matches!(status, "new" | "applied" | "dismissed") {
            anyhow::bail!("invalid status '{status}' (expected new|applied|dismissed)");
        }
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE jobs SET status = ?, status_changed_at = ?, status_note = ? WHERE id = ?",
            params![status, now, note, id],
        )?;
        if n == 0 {
            anyhow::bail!("no job with id {id}");
        }
        Ok(())
    }

    /// Bulk-update `status` (+ `status_note`) on every open job belonging to
    /// `company_id`. Closed jobs are intentionally left alone — they don't
    /// show up in `list jobs` anyway, and dismissing them would muddy the
    /// historical record. Returns the number of rows updated.
    pub fn set_status_for_company(
        &self,
        company_id: i64,
        status: &str,
        note: Option<&str>,
    ) -> Result<usize> {
        if !matches!(status, "new" | "applied" | "dismissed") {
            anyhow::bail!("invalid status '{status}' (expected new|applied|dismissed)");
        }
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE jobs SET status = ?, status_changed_at = ?, status_note = ?
             WHERE company_id = ? AND closed_at IS NULL",
            params![status, now, note, company_id],
        )?;
        Ok(n)
    }

    /// Look up a company by slug, optionally narrowing by ATS kind. Returns
    /// every match. Caller decides what to do with 0 / N>1 results —
    /// e.g. `ajs mark company:slug` errors on ambiguity so the user can
    /// re-run with `company:kind/slug`.
    pub fn find_companies_by_slug(
        &self,
        slug: &str,
        kind_filter: Option<&str>,
    ) -> Result<Vec<CompanyMatch>> {
        let mut sql = String::from(
            "SELECT id, name, ats_kind, ats_slug FROM companies WHERE ats_slug = ?",
        );
        if kind_filter.is_some() {
            sql.push_str(" AND ats_kind = ?");
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<CompanyMatch> {
            Ok(CompanyMatch {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                slug: r.get(3)?,
            })
        };
        let rows: Vec<CompanyMatch> = if let Some(kind) = kind_filter {
            stmt.query_map(params![slug, kind], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![slug], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    }

    pub fn list_jobs(&self, limit: usize) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.name, j.title, COALESCE(j.location, ''), j.apply_url
               FROM jobs j JOIN companies c ON c.id = j.company_id
              WHERE j.closed_at IS NULL
              ORDER BY j.last_seen DESC
              LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Per-job status filter for `list_jobs_filtered`. Default hides dismissed
/// rows; the other two are explicit user requests.
pub enum StatusFilter {
    HideDismissed,
    All,
    AppliedOnly,
}

/// Idempotently add a column to an existing table. SQLite has `CREATE TABLE
/// IF NOT EXISTS` but no `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so we
/// inspect `PRAGMA table_info` first.
fn ensure_column(conn: &Connection, table: &str, column: &str, defn: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {defn}"))?;
    }
    Ok(())
}
