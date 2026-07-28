//! Database writer - persists review/scan results to `cora.db`.
//!
//! Uses the v5 schema tables: `reviews`, `findings`, `finding_events`.
//! All operations are best-effort (non-fatal on error) to never block a review.


use rusqlite::Connection;

use crate::engine::types::{ReviewIssue, TokenUsage};
use crate::engine::Severity;
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

/// Save a review/scan run to `cora.db`.
///
/// Opens its own connection (the global DB is single-writer safe for our
/// low-frequency writes). Returns the `review_id` on success, or `None` on
/// error (best-effort: caller continues regardless).
pub fn save_review_to_db(record: &ReviewRecord<'_>) -> Option<i64> {
    let conn = open_db().ok()?;
    let project_id = schema::get_or_create_project(&conn, record.project_root).ok()?;

    // Insert the review row.
    let (input_tokens, output_tokens, cost_usd) = record
        .tokens
        .map(|t| (t.input_tokens as i64, t.output_tokens as i64, t.estimated_cost_usd))
        .unwrap_or((0, 0, 0.0));

    conn.execute(
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
    )
    .ok()?;

    let review_id = conn.last_insert_rowid();

    // Insert each finding + an "opened" event.
    let mut stmt_findings = conn
        .prepare(
            "INSERT INTO findings
                (review_id, file_path, line_number, severity, issue_type, title, body,
                 suggested_fix, status, fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
        )
        .ok()?;

    let mut stmt_events = conn
        .prepare(
            "INSERT INTO finding_events (finding_id, event_type, note)
             VALUES (?1, 'opened', NULL)",
        )
        .ok()?;

    for issue in record.issues {
        let fingerprint = compute_fingerprint(issue);
        stmt_findings
            .execute(rusqlite::params![
                review_id,
                issue.file,
                issue.line.map(|l| l as i64),
                issue.severity.to_string(),
                issue.issue_type.as_deref(),
                issue.title,
                issue.body,
                issue.suggested_fix.as_deref(),
                fingerprint,
            ])
            .ok()?;

        let finding_id = conn.last_insert_rowid();
        stmt_events
            .execute(rusqlite::params![finding_id])
            .ok()?;
    }

    Some(review_id)
}

/// Auto-resolve findings from prior reviews that no longer appear.
///
/// After saving a new review, any `open` findings in the same project whose
/// fingerprint is *not* in the current review's findings are marked `resolved`
/// with an `auto_resolved` event. Findings that *do* reappear are left `open`.
pub fn resolve_stale_findings(project_root: &str, current_fingerprints: &[String]) -> usize {
    let Ok(conn) = open_db() else { return 0 };
    let Ok(project_id) = schema::get_or_create_project(&conn, project_root) else { return 0 };

    // Fetch (id, fingerprint) for all open findings in this project.
    let mut stmt = match conn.prepare(
        "SELECT f.id, f.fingerprint FROM findings f
         JOIN reviews r ON f.review_id = r.id
         WHERE r.project_id = ?1
           AND f.status = 'open'
           AND f.fingerprint IS NOT NULL",
    ) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let mut rows = match stmt.query(rusqlite::params![project_id]) {
        Ok(r) => r,
        Err(_) => return 0,
    };

    let mut stale_ids: Vec<i64> = Vec::new();
    while let Some(row) = rows.next().unwrap_or(None) {
        let id: i64 = row.get(0).unwrap_or(0);
        let fp: String = row.get(1).unwrap_or_default();
        if !current_fingerprints.contains(&fp) {
            stale_ids.push(id);
        }
    }

    let mut resolved = 0;
    let mut stmt_update = conn
        .prepare("UPDATE findings SET status = 'resolved' WHERE id = ?1")
        .ok();
    let mut stmt_event = conn
        .prepare(
            "INSERT INTO finding_events (finding_id, event_type, note)
             VALUES (?1, 'auto_resolved', 'No longer found in latest review')",
        )
        .ok();

    for id in &stale_ids {
        if let (Some(u), Some(e)) = (stmt_update.as_mut(), stmt_event.as_mut()) {
            if u.execute(rusqlite::params![id]).is_ok()
                && e.execute(rusqlite::params![id]).is_ok()
            {
                resolved += 1;
            }
        }
    }

    resolved
}

/// Open the global `cora.db` and ensure migrations are up to date.
fn open_db() -> anyhow::Result<Connection> {
    crate::data_dir::ensure_data_dir()?;
    let db_path = crate::data_dir::graph_db_path();
    let conn = Connection::open(&db_path)?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    schema::run_migrations(&conn)?;
    Ok(conn)
}

/// Compute a fingerprint for dedup/auto-resolve: `file:line:title_slug`.
/// Public wrapper so callers (review.rs, scan.rs) can compute fingerprints
/// for the current review before calling `resolve_stale_findings`.
pub fn compute_fingerprint_pub(issue: &ReviewIssue) -> String {
    compute_fingerprint(issue)
}

fn compute_fingerprint(issue: &ReviewIssue) -> String {
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

    fn make_issue(file: &str, line: u32, severity: Severity, title: &str) -> ReviewIssue {
        ReviewIssue {
            file: file.to_string(),
            line: Some(line),
            severity,
            issue_type: Some("security".to_string()),
            title: title.to_string(),
            body: "test body".to_string(),
            suggested_fix: Some("fix it".to_string()),
        }
    }

    #[test]
    fn test_fingerprint_format() {
        let issue = make_issue("src/main.rs", 42, Severity::Critical, "SQL Injection");
        let fp = compute_fingerprint(&issue);
        assert_eq!(fp, "src/main.rs:42:sql_injection");
    }

    #[test]
    fn test_fingerprint_no_line() {
        let mut issue = make_issue("src/lib.rs", 0, Severity::Minor, "Unused Import");
        issue.line = None;
        let fp = compute_fingerprint(&issue);
        assert_eq!(fp, "src/lib.rs:0:unused_import");
    }

    #[test]
    fn test_score_no_issues() {
        let issues: Vec<ReviewIssue> = vec![];
        assert_eq!(calculate_score(&issues), 100.0);
    }

    #[test]
    fn test_score_with_critical() {
        let issues = vec![make_issue("a.rs", 1, Severity::Critical, "x")];
        assert_eq!(calculate_score(&issues), 80.0);
    }

    #[test]
    fn test_score_floor_zero() {
        let issues = vec![
            make_issue("a.rs", 1, Severity::Critical, "x"),
            make_issue("a.rs", 2, Severity::Critical, "y"),
            make_issue("a.rs", 3, Severity::Critical, "z"),
            make_issue("a.rs", 4, Severity::Critical, "w"),
            make_issue("a.rs", 5, Severity::Critical, "v"),
            make_issue("a.rs", 6, Severity::Critical, "u"),
        ];
        assert_eq!(calculate_score(&issues), 0.0);
    }

    #[test]
    fn test_score_mixed() {
        let issues = vec![
            make_issue("a.rs", 1, Severity::Critical, "c"),
            make_issue("b.rs", 2, Severity::Major, "m"),
            make_issue("c.rs", 3, Severity::Minor, "n"),
            make_issue("d.rs", 4, Severity::Info, "i"),
        ];
        // 100 - 20 - 10 - 3 - 1 = 66
        assert_eq!(calculate_score(&issues), 66.0);
    }
}
