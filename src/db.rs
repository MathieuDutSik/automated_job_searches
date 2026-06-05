use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::ats::AtsKind;

const SCHEMA: &str = r#"
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
    id           INTEGER PRIMARY KEY,
    company_id   INTEGER NOT NULL REFERENCES companies(id),
    ats_kind     TEXT NOT NULL,
    external_id  TEXT NOT NULL,
    title        TEXT NOT NULL,
    location     TEXT,
    remote       INTEGER,
    department   TEXT,
    apply_url    TEXT NOT NULL,
    description  TEXT,
    posted_at    TEXT,
    first_seen   TEXT NOT NULL,
    last_seen    TEXT NOT NULL,
    closed_at    TEXT,
    raw_json     TEXT NOT NULL,
    UNIQUE(ats_kind, external_id)
);

CREATE INDEX IF NOT EXISTS idx_jobs_open    ON jobs(closed_at) WHERE closed_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_jobs_company ON jobs(company_id);

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

pub struct Db {
    conn: Connection,
}

pub struct JobUpsert<'a> {
    pub company_id: i64,
    pub kind: AtsKind,
    pub external_id: &'a str,
    pub title: &'a str,
    pub location: Option<&'a str>,
    pub department: Option<&'a str>,
    pub apply_url: &'a str,
    pub posted_at: Option<&'a str>,
    pub raw_json: &'a str,
}

impl Db {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(&path)
            .with_context(|| format!("opening sqlite at {}", path.as_ref().display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
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
        let (id, is_new) = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE jobs SET last_seen = ?, title = ?, location = ?, department = ?, apply_url = ?, posted_at = ?, closed_at = NULL WHERE id = ?",
                    params![now, j.title, j.location, j.department, j.apply_url, j.posted_at, id],
                )?;
                (id, false)
            }
            None => {
                tx.execute(
                    "INSERT INTO jobs (company_id, ats_kind, external_id, title, location, department, apply_url, posted_at, first_seen, last_seen, raw_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![j.company_id, j.kind.as_str(), j.external_id, j.title, j.location, j.department, j.apply_url, j.posted_at, now, now, j.raw_json],
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
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn list_by_company(
        &self,
        limit_companies: usize,
    ) -> Result<Vec<(String, String, String, Vec<(String, String, String)>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, ats_kind, ats_slug FROM companies
             WHERE id IN (SELECT DISTINCT company_id FROM jobs WHERE closed_at IS NULL)
             ORDER BY name COLLATE NOCASE
             LIMIT ?",
        )?;
        let companies: Vec<(i64, String, String, String)> = stmt
            .query_map(params![limit_companies as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(companies.len());
        let mut jobs_stmt = self.conn.prepare(
            "SELECT title, COALESCE(location, ''), apply_url
               FROM jobs WHERE company_id = ? AND closed_at IS NULL
               ORDER BY title COLLATE NOCASE",
        )?;
        for (id, name, kind, slug) in companies {
            let jobs = jobs_stmt
                .query_map(params![id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            out.push((name, kind, slug, jobs));
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
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
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
