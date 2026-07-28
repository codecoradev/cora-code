//! Debt Tracker — aggregates review findings across runs and tracks quality trends.
//!
//! Stores lightweight JSON snapshots in `.cora/history/` after each review.
//! Provides aggregation and trend analysis across multiple reviews.

use crate::engine::quality_gate::GateResult;
use crate::engine::types::{ReviewIssue, Severity};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

// ─── Default config values ───

const DEFAULT_HISTORY_DIR: &str = ".cora/history";
const DEFAULT_RETENTION_DAYS: u64 = 90;

// ─── Category mapping ───

/// Map raw `issue_type` strings from LLM output to normalized tracking categories.
fn normalize_category(raw: &str) -> &'static str {
    let lower = raw.to_lowercase();
    match lower.as_str() {
        "security" | "vulnerability" | "injection" | "xss" | "csrf" | "sqli" => "security",
        "bug" | "bugs" | "logic" | "correctness" | "bug_risk" => "bug_risk",
        "performance" | "perf" | "memory" | "speed" | "n+1" => "performance",
        "error" | "panic" | "unwrap" | "error_handling" => "error_handling",
        "complexity" | "cognitive" | "complex" => "complexity",
        "style" | "naming" | "formatting" => "style",
        "best_practice" | "best-practice" | "bestpractice" => "best_practice",
        _ => "other",
    }
}

/// Build category → count map from a list of issues.
fn count_by_category(issues: &[ReviewIssue]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for issue in issues {
        let cat = issue
            .issue_type
            .as_deref()
            .map(normalize_category)
            .unwrap_or("other");
        *counts.entry(cat.to_string()).or_insert(0) += 1;
    }
    counts
}

/// Build severity → count map from a list of issues.
fn count_by_severity(issues: &[ReviewIssue]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for issue in issues {
        let sev = issue.severity.to_string();
        *counts.entry(sev).or_insert(0) += 1;
    }
    counts
}

// ─── DebtSnapshot — one file per review run ───

/// A single review snapshot stored in `.cora/history/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtSnapshot {
    /// ISO-8601 timestamp of the review.
    pub timestamp: DateTime<Utc>,
    /// Git commit hash (short).
    pub commit: Option<String>,
    /// Git branch name.
    pub branch: Option<String>,
    /// Number of files reviewed.
    pub files_reviewed: usize,
    /// Number of lines reviewed (if known).
    pub lines_reviewed: Option<usize>,
    /// Finding counts by severity.
    pub findings: HashMap<String, usize>,
    /// Finding counts by category.
    pub categories: HashMap<String, usize>,
    /// Quality score 0-10 (derived from severity distribution).
    pub quality_score: f64,
    /// Quality gate status: "passed", "failed", or "disabled".
    pub gate_status: String,
    /// Review duration in milliseconds.
    pub duration_ms: Option<u64>,
}

impl DebtSnapshot {
    /// Build a snapshot from review results.
    ///
    /// `files_reviewed` and `lines_reviewed` are best-effort (may be 0/None).
    /// `duration_ms` is the wall-clock time for the review call.
    pub fn from_review(
        issues: &[ReviewIssue],
        gate_result: Option<&GateResult>,
        commit: Option<String>,
        branch: Option<String>,
        files_reviewed: usize,
        lines_reviewed: Option<usize>,
        duration_ms: Option<u64>,
    ) -> Self {
        let findings = count_by_severity(issues);
        let categories = count_by_category(issues);
        let quality_score = calculate_quality_score(issues);
        let gate_status = gate_result
            .map(|g| g.status.to_string().to_lowercase())
            .unwrap_or_else(|| "disabled".to_string());

        Self {
            timestamp: Utc::now(),
            commit,
            branch,
            files_reviewed,
            lines_reviewed,
            findings,
            categories,
            quality_score,
            gate_status,
            duration_ms,
        }
    }
}

/// Calculate a quality score from 0-10 based on issue severities.
///
/// 10 = no issues. Each finding reduces the score:
/// - critical: -2.0
/// - major: -1.0
/// - minor: -0.3
/// - info: -0.1
fn calculate_quality_score(issues: &[ReviewIssue]) -> f64 {
    let mut score: f64 = 10.0;
    for issue in issues {
        let penalty: f64 = match issue.severity {
            Severity::Critical => 2.0,
            Severity::Major => 1.0,
            Severity::Minor => 0.3,
            Severity::Info => 0.1,
        };
        score -= penalty;
    }
    if score < 0.0 { 0.0 } else { score }
}

// ─── DebtReport — aggregated across snapshots ───

/// Aggregated debt report from multiple snapshots.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtReport {
    /// Number of reviews analyzed.
    pub reviews_analyzed: usize,
    /// Total findings across all reviews.
    pub total_findings: usize,
    /// Change in total findings vs previous period (negative = improving).
    pub change_from_previous: i64,
    /// Overall trend: "improving", "stable", or "worsening".
    pub trend: String,
    /// Average quality score across reviews.
    pub quality_score_avg: f64,
    /// Change in quality score vs previous period.
    pub quality_score_change: f64,
    /// Aggregated findings by severity.
    pub findings: HashMap<String, usize>,
    /// Per-category breakdown with trend.
    pub categories: Vec<CategoryReport>,
    /// Earliest snapshot timestamp.
    pub period_start: Option<DateTime<Utc>>,
    /// Latest snapshot timestamp.
    pub period_end: Option<DateTime<Utc>>,
}

/// Per-category report with trend indicator.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryReport {
    /// Category name.
    pub name: String,
    /// Total count across all reviews.
    pub count: usize,
    /// Change vs previous period.
    pub change: i64,
    /// Trend: "improving", "stable", "worsening".
    pub trend: String,
}

// ─── Trend calculation ───

/// Trend direction based on change value.
/// Trend direction based on change value.
#[allow(dead_code)]
fn trend_from_change(change: i64) -> &'static str {
    if change < -1 {
        "improving"
    } else if change > 1 {
        "worsening"
    } else {
        "stable"
    }
}

// ─── File I/O ───

/// Generate a filename for a snapshot using a content-based hash for uniqueness.
/// Format: `YYYY-MM-DD_{short_hash}_{content_hash}.json`
fn snapshot_filename(snapshot: &DebtSnapshot) -> String {
    let date = snapshot.timestamp.format("%Y-%m-%d");
    let hash = snapshot.commit.as_deref().unwrap_or("unknown");
    // Take first 7 chars of commit hash
    let short_hash = if hash.len() > 7 { &hash[..7] } else { hash };
    // Content-based uniqueness: hash the findings + categories + timestamp
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    snapshot.timestamp.hash(&mut hasher);
    {
        let mut findings_sorted: Vec<_> = snapshot.findings.iter().collect();
        findings_sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in findings_sorted {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        let mut categories_sorted: Vec<_> = snapshot.categories.iter().collect();
        categories_sorted.sort_by_key(|(k, _)| *k);
        for (k, v) in categories_sorted {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
    }
    let content_hash = format!("{:08x}", hasher.finish());
    format!("{date}_{short_hash}_{content_hash}.json")
}

/// Resolve the history directory path.
/// Checks for `.cora/history` relative to CWD.
fn history_dir(custom_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = custom_dir {
        PathBuf::from(dir)
    } else {
        PathBuf::from(DEFAULT_HISTORY_DIR)
    }
}

/// Save a snapshot to the history directory.
///
/// Creates the directory if it doesn't exist. Best-effort — errors are logged
/// but don't fail the review.
pub fn save_snapshot(snapshot: &DebtSnapshot, custom_dir: Option<&str>) {
    let dir = history_dir(custom_dir);

    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("failed to create history directory {}: {e}", dir.display());
        return;
    }

    let filename = snapshot_filename(snapshot);
    let path = dir.join(&filename);

    match serde_json::to_string_pretty(snapshot) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!("failed to write snapshot {}: {e}", path.display());
            } else {
                debug!("saved debt snapshot: {}", path.display());
            }
        }
        Err(e) => {
            warn!("failed to serialize snapshot: {e}");
        }
    }
}

/// Load all snapshots from the history directory.
///
/// Returns snapshots sorted by timestamp (oldest first).
/// Malformed files are skipped with a warning.
/// Load all snapshots from the history directory.
///
/// Returns snapshots sorted by timestamp (oldest first).
/// Malformed files are skipped with a warning.
#[allow(dead_code)]
pub fn load_snapshots(custom_dir: Option<&str>) -> Vec<DebtSnapshot> {
    let dir = history_dir(custom_dir);

    if !dir.is_dir() {
        return Vec::new();
    }

    let mut snapshots = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<DebtSnapshot>(&content) {
                        Ok(snapshot) => snapshots.push(snapshot),
                        Err(e) => {
                            warn!("skipping malformed snapshot {}: {e}", path.display());
                        }
                    },
                    Err(e) => {
                        warn!("failed to read snapshot {}: {e}", path.display());
                    }
                }
            }
        }
    }

    // Sort oldest first
    snapshots.sort_by_key(|s| s.timestamp);
    snapshots
}

/// Clean up snapshots older than `retention_days`.
///
/// Returns the number of files removed.
/// Clean up snapshots older than `retention_days`.
///
/// Returns the number of files removed.
#[allow(dead_code)]
pub fn cleanup_old_snapshots(custom_dir: Option<&str>, retention_days: u64) -> usize {
    let dir = history_dir(custom_dir);

    if !dir.is_dir() {
        return 0;
    }

    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    let mut removed = 0;

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(snapshot) = serde_json::from_str::<DebtSnapshot>(&content) {
                        if snapshot.timestamp < cutoff && std::fs::remove_file(&path).is_ok() {
                            removed += 1;
                            debug!("removed old snapshot: {}", path.display());
                        }
                    }
                }
            }
        }
    }

    removed
}

// ─── Aggregation ───

/// Aggregate snapshots into a report.
///
/// If there are enough snapshots, splits into "recent" and "previous" halves
/// to calculate trends. Otherwise, reports totals with "stable" trends.
/// Aggregate snapshots into a report.
///
/// If there are enough snapshots, splits into "recent" and "previous" halves
/// to calculate trends. Otherwise, reports totals with "stable" trends.
#[allow(dead_code)]
pub fn aggregate(snapshots: &[DebtSnapshot]) -> DebtReport {
    if snapshots.is_empty() {
        return DebtReport {
            reviews_analyzed: 0,
            total_findings: 0,
            change_from_previous: 0,
            trend: "stable".to_string(),
            quality_score_avg: 0.0,
            quality_score_change: 0.0,
            findings: HashMap::new(),
            categories: Vec::new(),
            period_start: None,
            period_end: None,
        };
    }

    let period_start = snapshots.first().map(|s| s.timestamp);
    let period_end = snapshots.last().map(|s| s.timestamp);

    // Calculate totals across all snapshots
    let mut total_findings = 0;
    let mut all_findings: HashMap<String, usize> = HashMap::new();
    let mut all_categories: HashMap<String, usize> = HashMap::new();
    let mut quality_scores: Vec<f64> = Vec::new();

    for snapshot in snapshots {
        let snapshot_total: usize = snapshot.findings.values().sum();
        total_findings += snapshot_total;

        for (sev, count) in &snapshot.findings {
            *all_findings.entry(sev.clone()).or_insert(0) += count;
        }

        for (cat, count) in &snapshot.categories {
            *all_categories.entry(cat.clone()).or_insert(0) += count;
        }

        quality_scores.push(snapshot.quality_score);
    }

    let quality_score_avg = quality_scores.iter().sum::<f64>() / quality_scores.len() as f64;

    // Split for trend calculation (half-half)
    let mid = snapshots.len() / 2;
    let (previous, recent): (&[_], &[_]) = if snapshots.len() >= 2 {
        (&snapshots[..mid.max(1)], &snapshots[mid.max(1)..])
    } else {
        (&[], snapshots)
    };

    let previous_total: usize = previous
        .iter()
        .map(|s| s.findings.values().sum::<usize>())
        .sum();
    let recent_total: usize = recent
        .iter()
        .map(|s| s.findings.values().sum::<usize>())
        .sum();

    let change_from_previous = recent_total as i64 - previous_total as i64;
    let trend = trend_from_change(change_from_previous).to_string();

    let previous_avg_quality = if previous.is_empty() {
        quality_score_avg
    } else {
        previous.iter().map(|s| s.quality_score).sum::<f64>() / previous.len() as f64
    };
    let recent_avg_quality = if recent.is_empty() {
        quality_score_avg
    } else {
        recent.iter().map(|s| s.quality_score).sum::<f64>() / recent.len() as f64
    };
    let quality_score_change = recent_avg_quality - previous_avg_quality;

    // Per-category with trend
    let mut category_reports = Vec::new();
    let mut previous_categories: HashMap<String, usize> = HashMap::new();
    for snap in previous {
        for (cat, count) in &snap.categories {
            *previous_categories.entry(cat.clone()).or_insert(0) += count;
        }
    }

    // Build recent category counts separately for accurate delta
    let mut recent_categories: HashMap<String, usize> = HashMap::new();
    for snap in recent {
        for (cat, count) in &snap.categories {
            *recent_categories.entry(cat.clone()).or_insert(0) += count;
        }
    }

    // Collect all unique category names
    let all_cat_names: std::collections::HashSet<&String> = all_categories
        .keys()
        .chain(recent_categories.keys())
        .chain(previous_categories.keys())
        .collect();
    let mut all_cat_names: Vec<String> = all_cat_names.into_iter().cloned().collect();
    all_cat_names.sort();

    for cat_name in &all_cat_names {
        let count = recent_categories.get(cat_name).copied().unwrap_or(0);
        let prev_count = previous_categories.get(cat_name).copied().unwrap_or(0);
        let change = count as i64 - prev_count as i64;

        category_reports.push(CategoryReport {
            name: cat_name.clone(),
            count,
            change,
            trend: trend_from_change(change).to_string(),
        });
    }

    DebtReport {
        reviews_analyzed: snapshots.len(),
        total_findings,
        change_from_previous,
        trend,
        quality_score_avg,
        quality_score_change,
        findings: all_findings,
        categories: category_reports,
        period_start,
        period_end,
    }
}

// ─── DB-backed snapshot loading ───

/// Load review snapshots from `cora.db` for the given project root.
///
/// This is the preferred data source (SoT). Falls back gracefully if the DB
/// doesn't exist or has no reviews.
///
/// Converts the DB's 0-100 score to the 0-10 scale used by `DebtSnapshot`.
pub fn load_snapshots_from_db(project_root: &str) -> Vec<DebtSnapshot> {
    let Some(conn) = crate::engine::db_writer::open_db_for_read() else {
        return Vec::new();
    };

    let canonical = match std::path::Path::new(project_root).canonicalize() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => project_root.to_string(),
    };

    // Get project ID
    let project_id: i64 = match conn.query_row(
        "SELECT id FROM projects WHERE root_path = ?1",
        rusqlite::params![canonical],
        |row| row.get(0),
    ) {
        Ok(id) => id,
        Err(_) => return Vec::new(),
    };

    // Load all reviews for this project, ordered by created_at
    let mut stmt = match conn.prepare(
        "SELECT id, commit_hash, branch, files_scanned, lines_scanned,
                score, gate_status, created_at
         FROM reviews WHERE project_id = ?1 ORDER BY created_at ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut rows = match stmt.query(rusqlite::params![project_id]) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut snapshots = Vec::new();
    while let Some(row) = rows.next().unwrap_or(None) {
        let review_id: i64 = row.get(0).unwrap_or(0);
        let commit: Option<String> = row.get(1).unwrap_or(None);
        let branch: Option<String> = row.get(2).unwrap_or(None);
        let files_scanned: i64 = row.get(3).unwrap_or(0);
        let lines_scanned: i64 = row.get(4).unwrap_or(0);
        let db_score: f64 = row.get(5).unwrap_or(100.0);
        let gate_status: String = row.get(6).unwrap_or_else(|_| "disabled".to_string());
        let created_at: String = row.get(7).unwrap_or_default();

        // Parse timestamp
        let timestamp = DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(&created_at, "%Y-%m-%d %H:%M:%S")
                    .map(|ndt| ndt.and_utc())
            })
            .unwrap_or_else(|_| Utc::now());

        // Load findings for this review
        let findings = load_findings_for_review(&conn, review_id);
        let categories = load_categories_for_review(&conn, review_id);

        // Convert DB score (0-100) to DebtSnapshot scale (0-10)
        let quality_score = db_score / 10.0;

        snapshots.push(DebtSnapshot {
            timestamp,
            commit,
            branch,
            files_reviewed: files_scanned as usize,
            lines_reviewed: if lines_scanned > 0 {
                Some(lines_scanned as usize)
            } else {
                None
            },
            findings,
            categories,
            quality_score,
            gate_status,
            duration_ms: None, // not stored in DB
        });
    }

    snapshots
}

/// Load severity → count map for findings of a specific review.
fn load_findings_for_review(conn: &rusqlite::Connection, review_id: i64) -> HashMap<String, usize> {
    let mut findings: HashMap<String, usize> = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT severity, COUNT(*) as cnt FROM findings
         WHERE review_id = ?1 AND status = 'open'
         GROUP BY severity",
    ) {
        Ok(s) => s,
        Err(_) => return findings,
    };
    let mut rows = match stmt.query(rusqlite::params![review_id]) {
        Ok(r) => r,
        Err(_) => return findings,
    };
    while let Some(row) = rows.next().unwrap_or(None) {
        let severity: String = row.get::<_, String>(0).unwrap_or_default().to_lowercase();
        let count: usize = row.get::<_, i64>(1).unwrap_or(0) as usize;
        *findings.entry(severity).or_insert(0) += count;
    }
    findings
}

/// Load category → count map for findings of a specific review.
fn load_categories_for_review(
    conn: &rusqlite::Connection,
    review_id: i64,
) -> HashMap<String, usize> {
    let mut categories: HashMap<String, usize> = HashMap::new();
    let mut stmt = match conn.prepare(
        "SELECT issue_type, COUNT(*) as cnt FROM findings
         WHERE review_id = ?1 AND status = 'open' AND issue_type IS NOT NULL
         GROUP BY issue_type",
    ) {
        Ok(s) => s,
        Err(_) => return categories,
    };
    let mut rows = match stmt.query(rusqlite::params![review_id]) {
        Ok(r) => r,
        Err(_) => return categories,
    };
    while let Some(row) = rows.next().unwrap_or(None) {
        let issue_type: String = row.get::<_, String>(0).unwrap_or_default();
        let count: usize = row.get::<_, i64>(1).unwrap_or(0) as usize;
        let cat = normalize_category(&issue_type);
        *categories.entry(cat.to_string()).or_insert(0) += count;
    }
    categories
}

// ─── Debt config ───

/// Debt tracking configuration — parsed from `.cora.yaml` under `debt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtConfig {
    /// Enable debt tracking (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Directory to store history snapshots (default: `.cora/history`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_dir: Option<String>,

    /// Auto-cleanup snapshots older than N days (default: 90).
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
}

fn default_true() -> bool {
    true
}

impl Default for DebtConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history_dir: None,
            retention_days: DEFAULT_RETENTION_DAYS,
        }
    }
}

fn default_retention_days() -> u64 {
    DEFAULT_RETENTION_DAYS
}

impl DebtConfig {
    /// Get the effective history directory.
    #[allow(dead_code)]
    pub fn history_dir(&self) -> &str {
        self.history_dir.as_deref().unwrap_or(DEFAULT_HISTORY_DIR)
    }

    /// Get the effective retention days.
    #[allow(dead_code)]
    pub fn retention_days(&self) -> u64 {
        self.retention_days
    }
}

// ─── Debt estimation ───

/// Estimated fix time in hours per severity level.
const DEBT_HOURS_CRITICAL: f64 = 2.0;
const DEBT_HOURS_MAJOR: f64 = 1.0;
const DEBT_HOURS_MINOR: f64 = 0.25;
const DEBT_HOURS_INFO: f64 = 0.1;

/// Estimate total fix time from severity counts.
#[allow(dead_code)]
pub fn estimate_debt_hours(findings: &HashMap<String, usize>) -> f64 {
    let critical = findings.get("critical").copied().unwrap_or(0) as f64;
    let major = findings.get("major").copied().unwrap_or(0) as f64;
    let minor = findings.get("minor").copied().unwrap_or(0) as f64;
    let info = findings.get("info").copied().unwrap_or(0) as f64;

    critical * DEBT_HOURS_CRITICAL
        + major * DEBT_HOURS_MAJOR
        + minor * DEBT_HOURS_MINOR
        + info * DEBT_HOURS_INFO
}

/// Format debt hours as human-readable string.
#[allow(dead_code)]
pub fn format_debt_hours(hours: f64) -> String {
    if hours < 1.0 {
        format!("~{:.0}min", hours * 60.0)
    } else if hours < 8.0 {
        format!("~{:.1}h", hours)
    } else {
        format!("~{:.1}d", hours / 8.0)
    }
}

// ─── Badge generation ───

/// Generate a shields.io-compatible JSON badge endpoint for quality score.
///
/// Output can be hosted as a static JSON file and used with:
/// `![quality](https://img.shields.io/endpoint?url=...badge.json)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BadgeData {
    /// Schema version (always 1).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Badge label.
    pub label: String,
    /// Badge message (score + trend).
    pub message: String,
    /// Badge color: green/yellow/red.
    pub color: String,
}

#[allow(dead_code)]
pub fn generate_badge(report: &DebtReport) -> BadgeData {
    let score = report.quality_score_avg;
    let trend_arrow = match report.trend.as_str() {
        "improving" => " ▼", // fewer findings = improving
        "worsening" => " ▲", // more findings = worsening
        _ => "",
    };
    let color = if score >= 8.0 {
        "green"
    } else if score >= 5.0 {
        "yellow"
    } else {
        "red"
    };

    BadgeData {
        schema_version: 1,
        label: "code quality".to_string(),
        message: format!("{:.1}/10{trend_arrow}", score),
        color: color.to_string(),
    }
}

/// Generate a badge that includes debt estimation.
#[allow(dead_code)]
pub fn generate_badge_with_debt(report: &DebtReport) -> BadgeData {
    let score = report.quality_score_avg;
    let hours = estimate_debt_hours(&report.findings);
    let human = format_debt_hours(hours);
    let trend_arrow = match report.trend.as_str() {
        "improving" => " ▼", // fewer findings = improving
        "worsening" => " ▲", // more findings = worsening
        _ => "",
    };
    let color = if score >= 8.0 {
        "green"
    } else if score >= 5.0 {
        "yellow"
    } else {
        "red"
    };

    BadgeData {
        schema_version: 1,
        label: "code quality".to_string(),
        message: format!("{:.1}/10 · debt {human}{trend_arrow}", score),
        color: color.to_string(),
    }
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(severity: Severity, issue_type: &str) -> ReviewIssue {
        ReviewIssue {
            file: "test.rs".to_string(),
            line: Some(1),
            severity,
            issue_type: Some(issue_type.to_string()),
            title: "test issue".to_string(),
            body: String::new(),
            suggested_fix: None,
        }
    }

    // ─── normalize_category ───

    #[test]
    fn normalize_security() {
        assert_eq!(normalize_category("security"), "security");
        assert_eq!(normalize_category("vulnerability"), "security");
        assert_eq!(normalize_category("injection"), "security");
        assert_eq!(normalize_category("Security"), "security");
    }

    #[test]
    fn normalize_bug_risk() {
        assert_eq!(normalize_category("bug"), "bug_risk");
        assert_eq!(normalize_category("bugs"), "bug_risk");
        assert_eq!(normalize_category("logic"), "bug_risk");
        assert_eq!(normalize_category("Bug"), "bug_risk");
    }

    #[test]
    fn normalize_performance() {
        assert_eq!(normalize_category("performance"), "performance");
        assert_eq!(normalize_category("perf"), "performance");
        assert_eq!(normalize_category("memory"), "performance");
    }

    #[test]
    fn normalize_error_handling() {
        assert_eq!(normalize_category("error"), "error_handling");
        assert_eq!(normalize_category("panic"), "error_handling");
        assert_eq!(normalize_category("unwrap"), "error_handling");
        assert_eq!(normalize_category("error_handling"), "error_handling");
    }

    #[test]
    fn normalize_best_practice() {
        assert_eq!(normalize_category("best_practice"), "best_practice");
        assert_eq!(normalize_category("best-practice"), "best_practice");
        assert_eq!(normalize_category("bestpractice"), "best_practice");
    }

    #[test]
    fn normalize_unknown() {
        assert_eq!(normalize_category("something_else"), "other");
        assert_eq!(normalize_category("random"), "other");
    }

    // ─── count_by_category ───

    #[test]
    fn count_categories_empty() {
        let counts = count_by_category(&[]);
        assert!(counts.is_empty());
    }

    #[test]
    fn count_categories_mixed() {
        let issues = vec![
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Major, "security"),
            make_issue(Severity::Major, "performance"),
            make_issue(Severity::Minor, "bug"),
            make_issue(Severity::Info, "style"),
        ];
        let counts = count_by_category(&issues);
        assert_eq!(counts.get("security"), Some(&2));
        assert_eq!(counts.get("performance"), Some(&1));
        assert_eq!(counts.get("bug_risk"), Some(&1));
        assert_eq!(counts.get("style"), Some(&1));
    }

    // ─── count_by_severity ───

    #[test]
    fn count_severities() {
        let issues = vec![
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Critical, "bug"),
            make_issue(Severity::Major, "performance"),
            make_issue(Severity::Minor, "style"),
            make_issue(Severity::Info, "suggestion"),
        ];
        let counts = count_by_severity(&issues);
        assert_eq!(counts.get("critical"), Some(&2));
        assert_eq!(counts.get("major"), Some(&1));
        assert_eq!(counts.get("minor"), Some(&1));
        assert_eq!(counts.get("info"), Some(&1));
    }

    // ─── calculate_quality_score ───

    #[test]
    fn quality_score_no_issues() {
        assert!((calculate_quality_score(&[]) - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_score_one_critical() {
        let issues = vec![make_issue(Severity::Critical, "security")];
        assert!((calculate_quality_score(&issues) - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_score_one_major() {
        let issues = vec![make_issue(Severity::Major, "bug")];
        assert!((calculate_quality_score(&issues) - 9.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_score_one_minor() {
        let issues = vec![make_issue(Severity::Minor, "style")];
        assert!((calculate_quality_score(&issues) - 9.7).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_score_one_info() {
        let issues = vec![make_issue(Severity::Info, "suggestion")];
        assert!((calculate_quality_score(&issues) - 9.9).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_score_clamped_at_zero() {
        let issues: Vec<ReviewIssue> = (0..6)
            .map(|_| make_issue(Severity::Critical, "security"))
            .collect();
        assert!((calculate_quality_score(&issues) - 0.0).abs() < f64::EPSILON);
    }

    // ─── DebtSnapshot::from_review ───

    #[test]
    fn snapshot_from_empty_review() {
        let snap = DebtSnapshot::from_review(
            &[],
            None,
            Some("abc1234".to_string()),
            Some("main".to_string()),
            5,
            Some(200),
            Some(1500),
        );
        assert!(snap.findings.is_empty());
        assert!(snap.categories.is_empty());
        assert!((snap.quality_score - 10.0).abs() < f64::EPSILON);
        assert_eq!(snap.gate_status, "disabled");
        assert_eq!(snap.commit.as_deref(), Some("abc1234"));
        assert_eq!(snap.branch.as_deref(), Some("main"));
        assert_eq!(snap.files_reviewed, 5);
        assert_eq!(snap.lines_reviewed, Some(200));
        assert_eq!(snap.duration_ms, Some(1500));
    }

    #[test]
    fn snapshot_from_review_with_issues() {
        let issues = vec![
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Major, "performance"),
            make_issue(Severity::Minor, "bug"),
        ];
        let snap = DebtSnapshot::from_review(
            &issues,
            None,
            Some("def5678".to_string()),
            None,
            3,
            None,
            None,
        );
        assert_eq!(snap.findings.get("critical"), Some(&1));
        assert_eq!(snap.findings.get("major"), Some(&1));
        assert_eq!(snap.findings.get("minor"), Some(&1));
        assert_eq!(snap.categories.get("security"), Some(&1));
        assert_eq!(snap.categories.get("performance"), Some(&1));
        assert_eq!(snap.categories.get("bug_risk"), Some(&1));
    }

    #[test]
    fn snapshot_with_gate_result() {
        use crate::engine::quality_gate::{GateStatus, SeverityCounts};

        let gate = crate::engine::quality_gate::GateResult {
            status: GateStatus::Fail,
            checks: vec![],
            severity_counts: SeverityCounts::default(),
            category_counts: HashMap::new(),
            total_findings: 1,
        };
        let snap = DebtSnapshot::from_review(
            &[make_issue(Severity::Critical, "security")],
            Some(&gate),
            None,
            None,
            1,
            None,
            None,
        );
        assert_eq!(snap.gate_status, "failed");
    }

    // ─── DebtSnapshot serialization ───

    #[test]
    fn snapshot_json_roundtrip() {
        let snap = DebtSnapshot::from_review(
            &[make_issue(Severity::Major, "performance")],
            None,
            Some("abc1234".to_string()),
            Some("develop".to_string()),
            10,
            Some(500),
            Some(3000),
        );
        let json = serde_json::to_string(&snap).unwrap();
        let back: DebtSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.commit, snap.commit);
        assert_eq!(back.branch, snap.branch);
        assert_eq!(back.files_reviewed, snap.files_reviewed);
        assert_eq!(back.lines_reviewed, snap.lines_reviewed);
        assert!((back.quality_score - snap.quality_score).abs() < f64::EPSILON);
        assert_eq!(back.gate_status, snap.gate_status);
    }

    // ─── snapshot_filename ───

    #[test]
    fn snapshot_filename_format() {
        let snap = DebtSnapshot::from_review(
            &[],
            None,
            Some("abcdef1234567890".to_string()),
            None,
            0,
            None,
            None,
        );
        let name = snapshot_filename(&snap);
        // Should be like: 2026-06-10_123456_abcdef1_000000000.json
        assert!(name.starts_with("2026"));
        assert!(name.contains("_abcdef1"));
        assert!(name.ends_with(".json"));
        // Should have 3 underscore-separated segments before .json
        let stem = name.trim_end_matches(".json");
        let parts: Vec<&str> = stem.split('_').collect();
        assert!(
            parts.len() >= 3,
            "expected at least 3 parts, got: {parts:?}"
        );
    }

    #[test]
    fn snapshot_filename_no_commit() {
        let snap = DebtSnapshot::from_review(&[], None, None, None, 0, None, None);
        let name = snapshot_filename(&snap);
        assert!(name.contains("unknown"));
    }

    // ─── trend_from_change ───

    #[test]
    fn trend_improving() {
        assert_eq!(trend_from_change(-5), "improving");
        assert_eq!(trend_from_change(-2), "improving");
    }

    #[test]
    fn trend_stable() {
        assert_eq!(trend_from_change(0), "stable");
        assert_eq!(trend_from_change(1), "stable");
        assert_eq!(trend_from_change(-1), "stable");
    }

    #[test]
    fn trend_worsening() {
        assert_eq!(trend_from_change(2), "worsening");
        assert_eq!(trend_from_change(10), "worsening");
    }

    // ─── aggregate ───

    #[test]
    fn aggregate_empty() {
        let report = aggregate(&[]);
        assert_eq!(report.reviews_analyzed, 0);
        assert_eq!(report.total_findings, 0);
        assert_eq!(report.trend, "stable");
    }

    #[test]
    fn aggregate_single_snapshot() {
        let issues = vec![
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Major, "bug"),
        ];
        let snap = DebtSnapshot::from_review(&issues, None, None, None, 1, None, None);
        let report = aggregate(&[snap]);
        assert_eq!(report.reviews_analyzed, 1);
        assert_eq!(report.total_findings, 2);
    }

    #[test]
    fn aggregate_multiple_snapshots_with_trend() {
        // Create 4 snapshots with improving trend
        let mut snapshots = Vec::new();

        // Earlier: 3 critical issues
        let mut snap1 = DebtSnapshot::from_review(
            &[
                make_issue(Severity::Critical, "security"),
                make_issue(Severity::Critical, "security"),
                make_issue(Severity::Critical, "security"),
            ],
            None,
            None,
            None,
            1,
            None,
            None,
        );
        snap1.timestamp = Utc::now() - chrono::Duration::days(4);

        // Later: 1 minor issue
        let mut snap2 = DebtSnapshot::from_review(
            &[make_issue(Severity::Minor, "style")],
            None,
            None,
            None,
            1,
            None,
            None,
        );
        snap2.timestamp = Utc::now() - chrono::Duration::days(3);

        let mut snap3 = DebtSnapshot::from_review(
            &[make_issue(Severity::Minor, "style")],
            None,
            None,
            None,
            1,
            None,
            None,
        );
        snap3.timestamp = Utc::now() - chrono::Duration::days(2);

        let mut snap4 = DebtSnapshot::from_review(
            &[make_issue(Severity::Info, "suggestion")],
            None,
            None,
            None,
            1,
            None,
            None,
        );
        snap4.timestamp = Utc::now() - chrono::Duration::days(1);

        snapshots.push(snap1);
        snapshots.push(snap2);
        snapshots.push(snap3);
        snapshots.push(snap4);

        let report = aggregate(&snapshots);
        assert_eq!(report.reviews_analyzed, 4);
        assert_eq!(report.total_findings, 6); // 3 + 1 + 1 + 1
    }

    // ─── DebtConfig defaults ───

    #[test]
    fn debt_config_default() {
        let config = DebtConfig::default();
        assert!(config.enabled);
        assert!(config.history_dir.is_none());
        assert_eq!(config.retention_days, 90);
    }

    #[test]
    fn debt_config_history_dir_default() {
        let config = DebtConfig::default();
        assert_eq!(config.history_dir(), ".cora/history");
    }

    #[test]
    fn debt_config_custom_history_dir() {
        let config = DebtConfig {
            history_dir: Some("custom/dir".to_string()),
            ..Default::default()
        };
        assert_eq!(config.history_dir(), "custom/dir");
    }

    // ─── save/load roundtrip ───

    #[test]
    fn save_and_load_snapshots() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let dir_str = tmp_dir.path().to_string_lossy().to_string();

        let snap1 = DebtSnapshot::from_review(
            &[make_issue(Severity::Critical, "security")],
            None,
            Some("aaa1111".to_string()),
            Some("main".to_string()),
            2,
            Some(100),
            Some(500),
        );

        let snap2 = DebtSnapshot::from_review(
            &[make_issue(Severity::Minor, "style")],
            None,
            Some("bbb2222".to_string()),
            Some("develop".to_string()),
            1,
            None,
            Some(300),
        );

        save_snapshot(&snap1, Some(&dir_str));
        save_snapshot(&snap2, Some(&dir_str));

        let loaded = load_snapshots(Some(&dir_str));
        assert_eq!(loaded.len(), 2);
        // Should be sorted by timestamp
        assert!(loaded[0].timestamp <= loaded[1].timestamp);
    }

    // ─── cleanup ───

    #[test]
    fn cleanup_removes_old_snapshots() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let dir_str = tmp_dir.path().to_string_lossy().to_string();

        // Create an old snapshot
        let mut old_snap = DebtSnapshot::from_review(
            &[make_issue(Severity::Critical, "security")],
            None,
            Some("old1111".to_string()),
            None,
            1,
            None,
            None,
        );
        old_snap.timestamp = Utc::now() - chrono::Duration::days(100);

        // Create a recent snapshot
        let recent_snap = DebtSnapshot::from_review(
            &[make_issue(Severity::Minor, "style")],
            None,
            Some("new2222".to_string()),
            None,
            1,
            None,
            None,
        );

        save_snapshot(&old_snap, Some(&dir_str));
        save_snapshot(&recent_snap, Some(&dir_str));

        // Cleanup with 90-day retention
        let removed = cleanup_old_snapshots(Some(&dir_str), 90);
        assert_eq!(removed, 1);

        // Only the recent one should remain
        let remaining = load_snapshots(Some(&dir_str));
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].commit.as_deref(), Some("new2222"));
    }

    // ─── estimate_debt_hours ───

    #[test]
    fn estimate_debt_empty() {
        let hours = estimate_debt_hours(&HashMap::new());
        assert!((hours - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn estimate_debt_critical_only() {
        let mut findings = HashMap::new();
        findings.insert("critical".to_string(), 3);
        let hours = estimate_debt_hours(&findings);
        assert!((hours - 6.0).abs() < f64::EPSILON); // 3 * 2.0
    }

    #[test]
    fn estimate_debt_mixed() {
        let mut findings = HashMap::new();
        findings.insert("critical".to_string(), 1);
        findings.insert("major".to_string(), 2);
        findings.insert("minor".to_string(), 4);
        findings.insert("info".to_string(), 3);
        let hours = estimate_debt_hours(&findings);
        // 1*2.0 + 2*1.0 + 4*0.25 + 3*0.1 = 2.0 + 2.0 + 1.0 + 0.3 = 5.3
        assert!((hours - 5.3).abs() < 0.001);
    }

    // ─── format_debt_hours ───

    #[test]
    fn format_debt_minutes() {
        assert_eq!(format_debt_hours(0.5), "~30min");
    }

    #[test]
    fn format_debt_hours_display() {
        assert_eq!(format_debt_hours(3.5), "~3.5h");
    }

    #[test]
    fn format_debt_days() {
        assert_eq!(format_debt_hours(16.0), "~2.0d");
    }

    // ─── generate_badge ───

    #[test]
    fn badge_high_score() {
        let report = aggregate(&[DebtSnapshot::from_review(
            &[],
            None,
            None,
            None,
            1,
            None,
            None,
        )]);
        let badge = generate_badge(&report);
        assert_eq!(badge.schema_version, 1);
        assert_eq!(badge.label, "code quality");
        assert!(badge.message.contains("10.0/10"));
        assert_eq!(badge.color, "green");
    }

    #[test]
    fn badge_medium_score() {
        let report = aggregate(&[DebtSnapshot::from_review(
            &[
                make_issue(Severity::Major, "bug"),
                make_issue(Severity::Major, "bug"),
                make_issue(Severity::Major, "bug"),
            ],
            None,
            None,
            None,
            1,
            None,
            None,
        )]);
        let badge = generate_badge(&report);
        // 10.0 - 3.0 = 7.0, which is >= 5.0 and < 8.0 = yellow
        assert_eq!(badge.color, "yellow");
    }

    #[test]
    fn badge_low_score() {
        let issues: Vec<ReviewIssue> = (0..6)
            .map(|_| make_issue(Severity::Critical, "security"))
            .collect();
        let report = aggregate(&[DebtSnapshot::from_review(
            &issues, None, None, None, 1, None, None,
        )]);
        let badge = generate_badge(&report);
        assert_eq!(badge.color, "red");
    }

    #[test]
    fn badge_serializes_with_rename() {
        let report = aggregate(&[DebtSnapshot::from_review(
            &[],
            None,
            None,
            None,
            1,
            None,
            None,
        )]);
        let badge = generate_badge(&report);
        let json = serde_json::to_string(&badge).unwrap();
        // Should use camelCase for shields.io compatibility
        assert!(json.contains("\"schemaVersion\":"));
    }
}
