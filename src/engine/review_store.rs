//! Review-history store: the one module that owns SQL for the `reviews`,
//! `findings` and `finding_events` tables (schema lives in `index/schema.rs`).
//!
//! # Shape
//!
//! [`ReviewStore`] wraps a borrowed [`Connection`], so every operation is
//! testable against an in-memory SQLite database. Commands (`cora findings`,
//! `cora debt`, review/scan persistence) call it and contain no SQL.
//!
//! # Error policy (decided here, in one place)
//!
//! * **Store methods return `Result`.** Callers that can act on a failure
//!   (the `cora findings` CLI) surface it.
//! * **Opening** ([`open_read`], [`open_write`]) returns `Result` too; the CLI
//!   prints its "could not open cora.db" message on `Err`.
//! * **Persisting a review is best-effort.** [`persist_review_best_effort`] is
//!   the single place that swallows write errors (logging at `debug`), so a
//!   failure to write history can never fail or block a review/scan.
//! * The read path used only for reporting ([`load_debt_rows`]) degrades to an
//!   empty result, the same as a missing database.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use tracing::debug;

use crate::engine::Severity;
use crate::engine::types::{ReviewIssue, TokenUsage};
use crate::index::schema;

/// Input data for saving a review/scan run to the database.
pub struct ReviewRecord<'a> {
    /// "review" or "scan".
    pub command: &'a str,
    /// Absolute path of the project root (used for project lookup/creation).
    pub project_root: &'a str,
    /// Git commit hash (short) if available.
    pub commit_hash: Option<&'a str>,
    /// Git branch name if available.
    pub branch: Option<&'a str>,
    /// LLM-generated summary text.
    pub summary: &'a str,
    /// Quality gate status: "passed", "failed", or "disabled".
    pub gate_status: &'a str,
    /// Number of files scanned/reviewed.
    pub files_scanned: usize,
    /// Number of lines scanned/reviewed.
    pub lines_scanned: usize,
    /// Whether the quality gate should block.
    pub should_block: bool,
    /// Token usage from the LLM call (if any).
    pub tokens: Option<&'a TokenUsage>,
    /// The issues/findings to persist.
    pub issues: &'a [ReviewIssue],
}

/// Filters for [`ReviewStore::list_findings`].
#[derive(Debug, Clone)]
pub struct FindingFilter {
    /// Include resolved/dismissed findings (default: open only).
    pub all: bool,
    /// Exact severity match, case-insensitive (stored lowercase).
    pub severity: Option<String>,
    /// Substring match on the file path.
    pub file: Option<String>,
    /// Maximum number of rows.
    pub limit: usize,
}

/// One row of `cora findings list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FindingRow {
    pub id: i64,
    pub severity: String,
    pub file_path: String,
    pub line_number: Option<i64>,
    pub title: String,
    pub status: String,
    pub fingerprint: Option<String>,
    pub created_at: String,
}

/// Counts for `cora findings stats`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindingStats {
    pub total: i64,
    pub open: i64,
    pub resolved: i64,
    pub dismissed: i64,
    pub reviews: i64,
}

/// Result of a manual status transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// No finding with that id.
    NotFound,
    /// Finding exists but is already in the target state; nothing written.
    Unchanged,
    /// Status updated and an audit event written.
    Applied,
}

/// A `reviews` row as needed by the debt tracker.
#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub id: i64,
    pub commit_hash: Option<String>,
    pub branch: Option<String>,
    pub files_scanned: i64,
    pub lines_scanned: i64,
    /// 0-100 score as stored.
    pub score: f64,
    pub gate_status: String,
    pub created_at: String,
    /// Open findings per severity (severity as stored -> count).
    pub open_by_severity: Vec<(String, usize)>,
    /// Open findings per raw `issue_type` -> count.
    pub open_by_issue_type: Vec<(String, usize)>,
}

/// Outcome of [`persist_review_best_effort`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PersistOutcome {
    /// `Some(review_id)` when the review row was written.
    pub review_id: Option<i64>,
    /// Number of stale findings auto-resolved.
    pub auto_resolved: usize,
}

// ─── Connection acquisition ───

/// Open the global `cora.db` read-only (no migrations, no PRAGMAs).
/// Errors if the database does not exist or cannot be opened.
pub fn open_read() -> Result<Connection> {
    open_read_at(&crate::data_dir::graph_db_path())
}

/// Read-only open of an arbitrary path (see [`open_read`]).
pub fn open_read_at(db_path: &Path) -> Result<Connection> {
    if !db_path.exists() {
        anyhow::bail!("{} does not exist", db_path.display());
    }
    Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} read-only", db_path.display()))
}

/// Open the global `cora.db` read-write, running migrations if needed.
pub fn open_write() -> Result<Connection> {
    crate::index::open_global_index()
}

// ─── Best-effort persistence (the single swallow-errors policy) ───

/// Persist a review/scan and auto-resolve findings that no longer appear.
///
/// Never fails: any error is logged at `debug` and reflected in the returned
/// [`PersistOutcome`]. Persisting history must not fail or block a review.
pub fn persist_review_best_effort(record: &ReviewRecord<'_>) -> PersistOutcome {
    match open_write() {
        Ok(conn) => persist_on(&conn, record),
        Err(e) => {
            debug!(error = %e, "could not open cora.db; review history not saved");
            PersistOutcome::default()
        }
    }
}

/// Best-effort persistence on a given connection (testable without `$HOME`).
pub fn persist_on(conn: &Connection, record: &ReviewRecord<'_>) -> PersistOutcome {
    let store = ReviewStore::new(conn);

    let review_id = match store.record_review(record) {
        Ok(id) => Some(id),
        Err(e) => {
            debug!(error = %e, "failed to save {} to cora.db", record.command);
            None
        }
    };

    // Resolution runs even if the insert failed (as it always has).
    let fps: Vec<String> = record.issues.iter().map(compute_fingerprint).collect();
    let auto_resolved = match store.resolve_stale(record.project_root, &fps) {
        Ok(n) => n,
        Err(e) => {
            debug!(error = %e, "failed to auto-resolve stale findings");
            0
        }
    };
    if auto_resolved > 0 {
        debug!(resolved = auto_resolved, "auto-resolved stale findings");
    }

    PersistOutcome {
        review_id,
        auto_resolved,
    }
}

/// Read review history for the debt tracker. Degrades to an empty list when
/// the database is missing/unreadable or the project has no reviews.
pub fn load_debt_rows(project_root: &str) -> Vec<ReviewRow> {
    let Ok(conn) = open_read() else {
        return Vec::new();
    };
    match ReviewStore::new(&conn).reviews_for_root(project_root) {
        Ok(rows) => rows,
        Err(e) => {
            debug!(error = %e, "could not read review history");
            Vec::new()
        }
    }
}

// ─── Store ───

/// All SQL for review history, over a borrowed connection.
pub struct ReviewStore<'a> {
    conn: &'a Connection,
}

impl<'a> ReviewStore<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a review row plus one `open` finding (and `opened` event) per
    /// issue, atomically. Returns the new `review_id`.
    pub fn record_review(&self, record: &ReviewRecord<'_>) -> Result<i64> {
        let project_id = schema::get_or_create_project(self.conn, record.project_root)?;

        let (input_tokens, output_tokens, cost_usd) = record
            .tokens
            .map(|t| {
                (
                    t.input_tokens as i64,
                    t.output_tokens as i64,
                    t.estimated_cost_usd,
                )
            })
            .unwrap_or((0, 0, 0.0));

        let tx = self.conn.unchecked_transaction()?;

        tx.execute(
            "INSERT INTO reviews
                (project_id, command, commit_hash, branch, summary, score, gate_status,
                 files_scanned, lines_scanned, should_block, input_tokens, output_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                project_id,
                record.command,
                record.commit_hash,
                record.branch,
                record.summary,
                calculate_score(record.issues) as i64,
                record.gate_status,
                record.files_scanned as i64,
                record.lines_scanned as i64,
                record.should_block as i64,
                input_tokens,
                output_tokens,
                cost_usd,
            ],
        )?;
        let review_id = tx.last_insert_rowid();

        {
            let mut stmt_findings = tx.prepare(
                "INSERT INTO findings
                    (review_id, file_path, line_number, severity, issue_type, title, body,
                     suggested_fix, status, fingerprint)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
            )?;
            let mut stmt_events = tx.prepare(
                "INSERT INTO finding_events (finding_id, event_type, note)
                 VALUES (?1, 'opened', NULL)",
            )?;

            for issue in record.issues {
                stmt_findings.execute(rusqlite::params![
                    review_id,
                    issue.file,
                    issue.line.map(|l| l as i64),
                    issue.severity.to_string(),
                    issue.issue_type.as_deref(),
                    issue.title,
                    issue.body,
                    issue.suggested_fix.as_deref(),
                    compute_fingerprint(issue),
                ])?;
                stmt_events.execute(rusqlite::params![tx.last_insert_rowid()])?;
            }
        }

        tx.commit()?;
        Ok(review_id)
    }

    /// Auto-resolve `open` findings of the project whose fingerprint is not in
    /// `current_fingerprints`, writing an `auto_resolved` event for each.
    /// Returns how many findings were resolved.
    pub fn resolve_stale(
        &self,
        project_root: &str,
        current_fingerprints: &[String],
    ) -> Result<usize> {
        let project_id = schema::get_or_create_project(self.conn, project_root)?;

        let candidates: Vec<(i64, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT f.id, f.fingerprint FROM findings f
                 JOIN reviews r ON f.review_id = r.id
                 WHERE r.project_id = ?1
                   AND f.status = 'open'
                   AND f.fingerprint IS NOT NULL",
            )?;
            stmt.query_map(rusqlite::params![project_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        let tx = self.conn.unchecked_transaction()?;
        let mut resolved = 0;
        {
            let mut update = tx.prepare("UPDATE findings SET status = 'resolved' WHERE id = ?1")?;
            let mut event = tx.prepare(
                "INSERT INTO finding_events (finding_id, event_type, note)
                 VALUES (?1, 'auto_resolved', 'No longer found in latest review')",
            )?;
            for (id, fp) in &candidates {
                if current_fingerprints.contains(fp) {
                    continue;
                }
                update.execute(rusqlite::params![id])?;
                event.execute(rusqlite::params![id])?;
                resolved += 1;
            }
        }
        tx.commit()?;
        Ok(resolved)
    }

    /// List findings (newest id first) joined with their review's timestamp.
    pub fn list_findings(&self, filter: &FindingFilter) -> Result<Vec<FindingRow>> {
        let mut sql = String::from(
            "SELECT f.id, f.severity, f.file_path, f.line_number, f.title, f.status,
                   f.fingerprint, r.created_at
            FROM findings f
            JOIN reviews r ON f.review_id = r.id",
        );

        // Parameterized placeholders only (no user text in the SQL string).
        let mut wheres: Vec<&str> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if !filter.all {
            wheres.push("f.status = 'open'");
        }
        if let Some(s) = &filter.severity {
            wheres.push("f.severity = ?");
            params.push(Box::new(s.to_lowercase()));
        }
        if let Some(f) = &filter.file {
            wheres.push("f.file_path LIKE ?");
            params.push(Box::new(format!("%{f}%")));
        }
        if !wheres.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&wheres.join(" AND "));
        }
        sql.push_str(" ORDER BY f.id DESC LIMIT ?");
        params.push(Box::new(filter.limit as i64));

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok(FindingRow {
                    id: r.get(0)?,
                    severity: r.get(1)?,
                    file_path: r.get(2)?,
                    line_number: r.get(3)?,
                    title: r.get(4)?,
                    status: r.get(5)?,
                    fingerprint: r.get(6)?,
                    created_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Aggregate counts over all findings and reviews.
    pub fn stats(&self) -> Result<FindingStats> {
        let count = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(FindingStats {
            total: count("SELECT count(*) FROM findings")?,
            open: count("SELECT count(*) FROM findings WHERE status = 'open'")?,
            resolved: count("SELECT count(*) FROM findings WHERE status = 'resolved'")?,
            dismissed: count("SELECT count(*) FROM findings WHERE status = 'dismissed'")?,
            reviews: count("SELECT count(*) FROM reviews")?,
        })
    }

    /// Current status of a finding, or `None` if it does not exist.
    pub fn finding_status(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status FROM findings WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Mark a finding dismissed and write a `dismissed` event. Dismissing an
    /// already-dismissed (or resolved) finding is allowed and re-recorded.
    pub fn dismiss(&self, id: i64, reason: Option<&str>) -> Result<Transition> {
        if self.finding_status(id)?.is_none() {
            return Ok(Transition::NotFound);
        }
        let note = reason.unwrap_or("Manually dismissed via CLI");
        self.transition(id, "dismissed", "dismissed", note)?;
        Ok(Transition::Applied)
    }

    /// Reopen a resolved/dismissed finding and write a `reopened` event.
    /// An already-open finding is left untouched ([`Transition::Unchanged`]).
    pub fn reopen(&self, id: i64) -> Result<Transition> {
        match self.finding_status(id)?.as_deref() {
            None => Ok(Transition::NotFound),
            Some("open") => Ok(Transition::Unchanged),
            Some(_) => {
                self.transition(id, "open", "reopened", "Manually reopened via CLI")?;
                Ok(Transition::Applied)
            }
        }
    }

    /// Status update + audit event in one transaction.
    fn transition(&self, id: i64, status: &str, event: &str, note: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE findings SET status = ?2 WHERE id = ?1",
            rusqlite::params![id, status],
        )?;
        tx.execute(
            "INSERT INTO finding_events (finding_id, event_type, note) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, event, note],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// All reviews of the project at `project_root` (oldest first) with their
    /// open-finding breakdowns. Empty when the project is unknown.
    pub fn reviews_for_root(&self, project_root: &str) -> Result<Vec<ReviewRow>> {
        let canonical = match Path::new(project_root).canonicalize() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => project_root.to_string(),
        };

        let project_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                rusqlite::params![canonical],
                |r| r.get(0),
            )
            .optional()?;
        let Some(project_id) = project_id else {
            return Ok(Vec::new());
        };

        let mut stmt = self.conn.prepare(
            "SELECT id, commit_hash, branch, files_scanned, lines_scanned,
                    score, gate_status, created_at
             FROM reviews WHERE project_id = ?1 ORDER BY created_at ASC",
        )?;
        let mut rows: Vec<ReviewRow> = stmt
            .query_map(rusqlite::params![project_id], |r| {
                Ok(ReviewRow {
                    id: r.get(0)?,
                    commit_hash: r.get(1)?,
                    branch: r.get(2)?,
                    files_scanned: r.get(3)?,
                    lines_scanned: r.get(4)?,
                    score: r.get(5)?,
                    gate_status: r.get(6)?,
                    created_at: r.get(7)?,
                    open_by_severity: Vec::new(),
                    open_by_issue_type: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        for row in &mut rows {
            row.open_by_severity = self.open_counts(
                "SELECT severity, COUNT(*) FROM findings
                 WHERE review_id = ?1 AND status = 'open'
                 GROUP BY severity",
                row.id,
            )?;
            row.open_by_issue_type = self.open_counts(
                "SELECT issue_type, COUNT(*) FROM findings
                 WHERE review_id = ?1 AND status = 'open' AND issue_type IS NOT NULL
                 GROUP BY issue_type",
                row.id,
            )?;
        }
        Ok(rows)
    }

    fn open_counts(&self, sql: &str, review_id: i64) -> Result<Vec<(String, usize)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let counts = stmt
            .query_map(rusqlite::params![review_id], |r| {
                let key: Option<String> = r.get(0)?;
                let n: i64 = r.get(1)?;
                Ok((key.unwrap_or_default(), n as usize))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(counts)
    }
}

// ─── Pure helpers ───

/// Compute a fingerprint for dedup/auto-resolve: `file:line:title_slug`.
pub fn compute_fingerprint(issue: &ReviewIssue) -> String {
    let line = issue.line.unwrap_or(0);
    let title_slug = issue.title.to_lowercase().replace(' ', "_");
    format!("{}:{}:{}", issue.file, line, title_slug)
}

/// Calculate a quality score 0-100 from issue severities.
///
/// 100 = no issues. Each finding reduces the score:
/// - critical: -20, major: -10, minor: -3, info: -1
fn calculate_score(issues: &[ReviewIssue]) -> f64 {
    let mut score: f64 = 100.0;
    for issue in issues {
        let penalty: f64 = match issue.severity {
            Severity::Critical => 20.0,
            Severity::Major => 10.0,
            Severity::Minor => 3.0,
            Severity::Info => 1.0,
        };
        score -= penalty;
    }
    score.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        schema::run_migrations(&conn).unwrap();
        conn
    }

    fn make_issue(file: &str, line: u32, severity: Severity, title: &str) -> ReviewIssue {
        ReviewIssue {
            rule_id: None,
            also_matches: Vec::new(),
            file: file.to_string(),
            line: Some(line),
            severity,
            issue_type: Some("security".to_string()),
            title: title.to_string(),
            body: "test body".to_string(),
            suggested_fix: Some("fix it".to_string()),
        }
    }

    fn record<'a>(root: &'a str, issues: &'a [ReviewIssue]) -> ReviewRecord<'a> {
        ReviewRecord {
            command: "review",
            project_root: root,
            commit_hash: Some("abc123"),
            branch: Some("main"),
            summary: "sum",
            gate_status: "passed",
            files_scanned: 3,
            lines_scanned: 40,
            should_block: false,
            tokens: None,
            issues,
        }
    }

    fn events(conn: &Connection, finding: i64) -> Vec<(String, Option<String>)> {
        let mut s = conn
            .prepare(
                "SELECT event_type, note FROM finding_events WHERE finding_id = ?1 ORDER BY id",
            )
            .unwrap();
        s.query_map([finding], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn filter(all: bool) -> FindingFilter {
        FindingFilter {
            all,
            severity: None,
            file: None,
            limit: 50,
        }
    }

    #[test]
    fn test_fingerprint_format() {
        let issue = make_issue("src/main.rs", 42, Severity::Critical, "SQL Injection");
        assert_eq!(compute_fingerprint(&issue), "src/main.rs:42:sql_injection");
        let mut issue = make_issue("src/lib.rs", 0, Severity::Minor, "Unused Import");
        issue.line = None;
        assert_eq!(compute_fingerprint(&issue), "src/lib.rs:0:unused_import");
    }

    #[test]
    fn test_score() {
        assert_eq!(calculate_score(&[]), 100.0);
        let issues = vec![
            make_issue("a.rs", 1, Severity::Critical, "c"),
            make_issue("b.rs", 2, Severity::Major, "m"),
            make_issue("c.rs", 3, Severity::Minor, "n"),
            make_issue("d.rs", 4, Severity::Info, "i"),
        ];
        assert_eq!(calculate_score(&issues), 66.0);
        let many: Vec<_> = (0..6)
            .map(|i| make_issue("a.rs", i, Severity::Critical, "x"))
            .collect();
        assert_eq!(calculate_score(&many), 0.0);
    }

    #[test]
    fn record_review_round_trip() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let issues = vec![
            make_issue("src/a.rs", 10, Severity::Critical, "Bad Thing"),
            make_issue("src/b.rs", 20, Severity::Minor, "Meh"),
        ];
        let id = store.record_review(&record("/proj", &issues)).unwrap();

        let (cmd, commit, score, gate, files): (String, String, i64, String, i64) = conn
            .query_row(
                "SELECT command, commit_hash, score, gate_status, files_scanned
                 FROM reviews WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (cmd.as_str(), commit.as_str(), score, gate.as_str(), files),
            ("review", "abc123", 77, "passed", 3)
        );

        let rows = store.list_findings(&filter(false)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].file_path, "src/b.rs"); // newest id first
        assert_eq!(
            rows[1].fingerprint.as_deref(),
            Some("src/a.rs:10:bad_thing")
        );
        assert_eq!(rows[1].severity, "critical"); // stored lowercase
        assert_eq!(rows[1].status, "open");
        for r in &rows {
            assert_eq!(events(&conn, r.id), vec![("opened".to_string(), None)]);
        }
    }

    #[test]
    fn record_review_is_atomic() {
        let conn = mem();
        conn.execute_batch("DROP TABLE finding_events").unwrap();
        let issues = vec![make_issue("a.rs", 1, Severity::Info, "x")];
        assert!(
            ReviewStore::new(&conn)
                .record_review(&record("/p", &issues))
                .is_err()
        );
        let n: i64 = conn
            .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "failed record must roll back the review row");
    }

    #[test]
    fn list_filters() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let issues = vec![
            make_issue("src/a.rs", 1, Severity::Critical, "one"),
            make_issue("lib/b.rs", 2, Severity::Minor, "two"),
            make_issue("src/c.rs", 3, Severity::Minor, "three"),
        ];
        store.record_review(&record("/p", &issues)).unwrap();
        store.dismiss(3, None).unwrap();

        assert_eq!(store.list_findings(&filter(false)).unwrap().len(), 2);
        assert_eq!(store.list_findings(&filter(true)).unwrap().len(), 3);

        let mut f = filter(true);
        f.file = Some("src/".into());
        assert_eq!(store.list_findings(&f).unwrap().len(), 2);
        f.all = false; // open only: #3 is dismissed
        let rows = store.list_findings(&f).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "one");

        // Severities are stored lowercase; the filter is case-insensitive.
        for arg in ["minor", "MINOR", "Minor"] {
            let mut f = filter(true);
            f.severity = Some(arg.into());
            assert_eq!(store.list_findings(&f).unwrap().len(), 2, "arg {arg}");
        }
        let mut f = filter(true);
        f.severity = Some("critical".into());
        assert_eq!(store.list_findings(&f).unwrap().len(), 1);
        f.severity = Some("bogus".into());
        assert_eq!(store.list_findings(&f).unwrap().len(), 0);

        let mut f = filter(true);
        f.limit = 1;
        let rows = store.list_findings(&f).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 3);
    }

    #[test]
    fn dismiss_and_reopen_transitions() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let issues = vec![make_issue("a.rs", 1, Severity::Major, "t")];
        store.record_review(&record("/p", &issues)).unwrap();

        assert_eq!(store.dismiss(99, None).unwrap(), Transition::NotFound);
        assert_eq!(store.reopen(99).unwrap(), Transition::NotFound);
        // already open: nothing written
        assert_eq!(store.reopen(1).unwrap(), Transition::Unchanged);
        assert_eq!(events(&conn, 1).len(), 1);

        assert_eq!(
            store.dismiss(1, Some("wontfix")).unwrap(),
            Transition::Applied
        );
        assert_eq!(
            store.finding_status(1).unwrap().as_deref(),
            Some("dismissed")
        );
        assert_eq!(
            events(&conn, 1).last().unwrap(),
            &("dismissed".to_string(), Some("wontfix".to_string()))
        );

        // dismissing again re-records (unchanged legacy behaviour), default note
        assert_eq!(store.dismiss(1, None).unwrap(), Transition::Applied);
        assert_eq!(
            events(&conn, 1).last().unwrap().1.as_deref(),
            Some("Manually dismissed via CLI")
        );

        assert_eq!(store.reopen(1).unwrap(), Transition::Applied);
        assert_eq!(store.finding_status(1).unwrap().as_deref(), Some("open"));
        assert_eq!(
            events(&conn, 1).last().unwrap(),
            &(
                "reopened".to_string(),
                Some("Manually reopened via CLI".to_string())
            )
        );
    }

    #[test]
    fn reopen_after_auto_resolve() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let issues = vec![make_issue("a.rs", 1, Severity::Major, "t")];
        store.record_review(&record("/p", &issues)).unwrap();
        assert_eq!(store.resolve_stale("/p", &[]).unwrap(), 1);
        assert_eq!(store.reopen(1).unwrap(), Transition::Applied);
        assert_eq!(store.stats().unwrap().open, 1);
    }

    #[test]
    fn resolve_stale_only_missing_fingerprints_in_project() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let a = make_issue("a.rs", 1, Severity::Major, "keep");
        let b = make_issue("b.rs", 2, Severity::Major, "gone");
        store.record_review(&record("/p", &[a.clone(), b])).unwrap();
        // another project's finding must be untouched
        store
            .record_review(&record(
                "/other",
                &[make_issue("z.rs", 9, Severity::Minor, "z")],
            ))
            .unwrap();

        let n = store
            .resolve_stale("/p", &[compute_fingerprint(&a)])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.finding_status(1).unwrap().as_deref(), Some("open"));
        assert_eq!(
            store.finding_status(2).unwrap().as_deref(),
            Some("resolved")
        );
        assert_eq!(store.finding_status(3).unwrap().as_deref(), Some("open"));
        assert_eq!(
            events(&conn, 2).last().unwrap(),
            &(
                "auto_resolved".to_string(),
                Some("No longer found in latest review".to_string())
            )
        );
    }

    #[test]
    fn stats_counts() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let zero = FindingStats {
            total: 0,
            open: 0,
            resolved: 0,
            dismissed: 0,
            reviews: 0,
        };
        assert_eq!(store.stats().unwrap(), zero);
        let issues = vec![
            make_issue("a.rs", 1, Severity::Major, "a"),
            make_issue("b.rs", 2, Severity::Major, "b"),
            make_issue("c.rs", 3, Severity::Major, "c"),
        ];
        store.record_review(&record("/p", &issues)).unwrap();
        store.dismiss(1, None).unwrap();
        store
            .resolve_stale("/p", &[compute_fingerprint(&issues[2])])
            .unwrap();
        assert_eq!(
            store.stats().unwrap(),
            FindingStats {
                total: 3,
                open: 1,
                resolved: 1,
                dismissed: 1,
                reviews: 1
            }
        );
    }

    #[test]
    fn debt_reads_against_store() {
        let conn = mem();
        let store = ReviewStore::new(&conn);
        let i1 = make_issue("a.rs", 1, Severity::Critical, "a");
        let mut i2 = make_issue("b.rs", 2, Severity::Minor, "b");
        i2.issue_type = None;
        let i3 = make_issue("c.rs", 3, Severity::Critical, "c");
        // nonexistent path: reviews_for_root falls back to the raw string
        store
            .record_review(&record("/nonexistent/proj", &[i1, i2, i3]))
            .unwrap();
        store.dismiss(3, None).unwrap(); // dismissed findings are not counted
        store
            .record_review(&record("/nonexistent/proj", &[]))
            .unwrap();

        let rows = store.reviews_for_root("/nonexistent/proj").unwrap();
        assert_eq!(rows.len(), 2);
        let r = &rows[0];
        assert_eq!((r.files_scanned, r.lines_scanned, r.score), (3, 40, 57.0));
        assert_eq!(r.commit_hash.as_deref(), Some("abc123"));
        let mut sev = r.open_by_severity.clone();
        sev.sort();
        assert_eq!(
            sev,
            vec![("critical".to_string(), 1), ("minor".to_string(), 1)]
        );
        assert_eq!(r.open_by_issue_type, vec![("security".to_string(), 1)]);
        assert!(rows[1].open_by_severity.is_empty());

        assert!(store.reviews_for_root("/unknown").unwrap().is_empty());
    }

    #[test]
    fn persist_records_and_auto_resolves() {
        let conn = mem();
        let old = make_issue("a.rs", 1, Severity::Major, "old");
        let r1 = persist_on(&conn, &record("/p", std::slice::from_ref(&old)));
        assert_eq!(r1.review_id, Some(1));
        assert_eq!(r1.auto_resolved, 0);

        let new = make_issue("b.rs", 2, Severity::Minor, "new");
        let r2 = persist_on(&conn, &record("/p", &[new]));
        assert_eq!(r2.review_id, Some(2));
        assert_eq!(r2.auto_resolved, 1);
        let store = ReviewStore::new(&conn);
        assert_eq!(
            store.finding_status(1).unwrap().as_deref(),
            Some("resolved")
        );
        assert_eq!(store.finding_status(2).unwrap().as_deref(), Some("open"));
    }

    #[test]
    fn persist_is_best_effort_when_write_fails() {
        // Broken schema: persisting must not panic or error, just report nothing.
        let conn = mem();
        conn.execute_batch("DROP TABLE finding_events; DROP TABLE findings;")
            .unwrap();
        let issues = vec![make_issue("a.rs", 1, Severity::Info, "x")];
        let out = persist_on(&conn, &record("/p", &issues));
        assert_eq!(out, PersistOutcome::default());
    }

    #[test]
    fn open_read_missing_db_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_read_at(&dir.path().join("nope.db")).is_err());
    }
}
