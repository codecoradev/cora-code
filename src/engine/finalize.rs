//! The result tail of a review: everything that happens to the merged findings
//! after the judge (LLM or deterministic scanners) is done (#648).
//!
//! One place owns:
//!
//! - the severity filter and the `should_block` decision,
//! - quality-gate evaluation,
//! - the exit code,
//! - the best-effort side effects (debt snapshot, `cora.db` history).
//!
//! Callers (`review`, chunked review, `commit`, `scan`, MCP) do formatting and
//! printing themselves and call [`shape`] first, then format, then the
//! `save_*` functions. `shape` is pure (no IO beyond gate evaluation), so it is
//! testable without an LLM; the `save_*` functions never fail a run.

use crate::config::schema::Config;
use crate::engine::debt_tracker::{self, DebtSnapshot};
use crate::engine::quality_gate::{self, GateResult, GateStatus};
use crate::engine::review_store::{self, ReviewRecord};
use crate::engine::types::{
    EXIT_BLOCKED, EXIT_OK, ReviewIssue, ReviewResponse, Severity, TokenUsage,
};

/// True when any issue is at or above `min` severity.
///
/// `Severity` orders Critical(0) < Major(1) < Minor(2) < Info(3), so "at or
/// above `min`" means `severity <= min`.
pub fn blocks(issues: &[ReviewIssue], min: Severity) -> bool {
    issues.iter().any(|i| i.severity <= min)
}

/// How [`shape`] should treat the findings.
#[derive(Debug, Clone, Copy)]
pub struct ShapeOptions {
    /// Findings below this severity are dropped (when `filter`) and never
    /// block.
    pub min_severity: Severity,
    /// Drop findings below `min_severity` from the list. `cora scan` keeps
    /// them all and only uses `min_severity` for the blocking decision.
    pub filter: bool,
    /// Evaluate the quality gate (when `quality_gate.enabled`).
    pub gate: bool,
    /// CI mode: any remaining finding blocks.
    pub ci: bool,
}

/// What [`shape`] decided besides the (mutated) findings.
#[derive(Debug, Clone)]
pub struct Shape {
    /// Quality-gate result, when the gate ran.
    pub gate: Option<GateResult>,
    /// Whether the run blocks (see [`exit_code`]).
    pub should_block: bool,
    /// Process exit code.
    pub exit_code: i32,
}

impl Shape {
    /// `passed` / `failed` / `disabled`, as stored in the review history.
    pub fn gate_label(&self) -> &'static str {
        match self.gate.as_ref().map(|g| g.status) {
            Some(GateStatus::Pass) => "passed",
            Some(GateStatus::Fail) => "failed",
            None => "disabled",
        }
    }

    /// True when the quality gate ran and failed.
    pub fn gate_failed(&self) -> bool {
        self.gate
            .as_ref()
            .is_some_and(|g| g.status == GateStatus::Fail)
    }
}

/// Filter `issues` in place, then decide gate, blocking and exit code against
/// the list the user will actually see (#312).
pub fn shape_issues(issues: &mut Vec<ReviewIssue>, config: &Config, opts: &ShapeOptions) -> Shape {
    if opts.filter {
        issues.retain(|i| i.severity <= opts.min_severity);
    }
    let should_block = blocks(issues, opts.min_severity);
    let gate = (opts.gate && config.quality_gate.enabled)
        .then(|| quality_gate::evaluate(issues, &config.quality_gate));
    let exit_code = exit_code(
        gate.as_ref().map(|g| g.status),
        opts.ci,
        !issues.is_empty(),
        should_block,
        config.hook.mode.as_str(),
    );
    Shape {
        gate,
        should_block,
        exit_code,
    }
}

/// [`shape_issues`] for a [`ReviewResponse`]: also stores the recomputed
/// `should_block` on the response.
pub fn shape(response: &mut ReviewResponse, config: &Config, opts: &ShapeOptions) -> Shape {
    let shaped = shape_issues(&mut response.issues, config, opts);
    response.should_block = shaped.should_block;
    shaped
}

/// Exit code from the gate status, CI flag, and the **filtered** findings.
///
/// | Code | Meaning |
/// |------|---------|
/// | 0 | Completed; no findings at or above the severity threshold |
/// | 2 | Findings are blocking: gate fail, CI with any issue, or hook in block mode with blocking severity |
pub fn exit_code(
    gate_status: Option<GateStatus>,
    ci: bool,
    has_issues: bool,
    should_block: bool,
    hook_mode: &str,
) -> i32 {
    if gate_status == Some(GateStatus::Fail) {
        return EXIT_BLOCKED;
    }
    if ci {
        return if has_issues { EXIT_BLOCKED } else { EXIT_OK };
    }
    if should_block && hook_mode == "block" {
        EXIT_BLOCKED
    } else {
        EXIT_OK
    }
}

/// Short commit hash and branch of the current checkout, cached per process.
fn git_context() -> (Option<String>, Option<String>) {
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<(Option<String>, Option<String>)> = LazyLock::new(|| {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());
        let branch = crate::git::get_current_branch().ok();
        (commit, branch)
    });

    (CONTEXT.0.clone(), CONTEXT.1.clone())
}

/// Save a debt-tracking snapshot (best-effort, no-op when `debt.enabled` is
/// off).
pub fn save_debt(
    config: &Config,
    issues: &[ReviewIssue],
    gate: Option<&GateResult>,
    files_reviewed: usize,
) {
    if !config.debt.enabled {
        return;
    }
    let (commit, branch) = git_context();
    let snapshot =
        DebtSnapshot::from_review(issues, gate, commit, branch, files_reviewed, None, None);
    debt_tracker::save_snapshot(&snapshot, config.debt.history_dir.as_deref());
}

/// What a run records in the review history.
#[derive(Debug, Clone, Copy)]
pub struct HistoryEntry<'a> {
    /// `review`, `review --ci`, `scan`, `commit`.
    pub command: &'a str,
    /// Summary text of the run.
    pub summary: &'a str,
    /// Files covered by the run (0 when not tracked).
    pub files_scanned: usize,
    /// Lines covered by the run (0 when not tracked).
    pub lines_scanned: usize,
    /// Token usage of the judge, if any.
    pub tokens: Option<&'a TokenUsage>,
}

/// Save the findings to `cora.db` (best-effort: a history-write failure never
/// fails the run).
pub fn save_history(entry: &HistoryEntry<'_>, issues: &[ReviewIssue], shaped: &Shape) {
    let (commit, branch) = git_context();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let record = ReviewRecord {
        command: entry.command,
        project_root: &cwd,
        commit_hash: commit.as_deref(),
        branch: branch.as_deref(),
        summary: entry.summary,
        gate_status: shaped.gate_label(),
        files_scanned: entry.files_scanned,
        lines_scanned: entry.lines_scanned,
        should_block: shaped.should_block,
        tokens: entry.tokens,
        issues,
    };
    review_store::persist_review_best_effort(&record);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(severity: Severity) -> ReviewIssue {
        ReviewIssue::new("src/main.rs", Some(1), severity, "test")
            .with_type("bug")
            .with_body("body")
    }

    fn opts(min: Severity) -> ShapeOptions {
        ShapeOptions {
            min_severity: min,
            filter: true,
            gate: true,
            ci: false,
        }
    }

    fn config(mode: &str, gate: bool) -> Config {
        let mut c = Config::default();
        c.hook.mode = mode.to_string();
        c.quality_gate.enabled = gate;
        c
    }

    #[test]
    fn blocks_is_at_or_above_min_severity() {
        let issues = vec![issue(Severity::Minor)];
        assert!(blocks(&issues, Severity::Minor));
        assert!(blocks(&issues, Severity::Info));
        assert!(!blocks(&issues, Severity::Major));
        assert!(!blocks(&[], Severity::Info));
    }

    // ─── #312: the decision matches what the user sees ───

    #[test]
    fn filter_drops_below_threshold_and_recomputes_should_block() {
        let mut issues = vec![issue(Severity::Minor), issue(Severity::Info)];
        let shaped = shape_issues(&mut issues, &config("block", false), &opts(Severity::Major));
        assert!(issues.is_empty());
        assert!(!shaped.should_block);
        assert_eq!(shaped.exit_code, EXIT_OK);
    }

    #[test]
    fn filter_keeps_blocking_findings() {
        let mut issues = vec![issue(Severity::Critical), issue(Severity::Info)];
        let shaped = shape_issues(&mut issues, &config("block", false), &opts(Severity::Major));
        assert_eq!(issues.len(), 1);
        assert!(shaped.should_block);
        assert_eq!(shaped.exit_code, EXIT_BLOCKED);
    }

    #[test]
    fn no_filter_keeps_every_issue_but_still_decides_blocking() {
        // `cora scan`: every finding is listed; only min_severity blocks.
        let mut issues = vec![issue(Severity::Info)];
        let o = ShapeOptions {
            filter: false,
            ..opts(Severity::Major)
        };
        let shaped = shape_issues(&mut issues, &config("block", false), &o);
        assert_eq!(issues.len(), 1);
        assert!(!shaped.should_block);
    }

    #[test]
    fn shape_stores_should_block_on_the_response() {
        let mut resp = ReviewResponse {
            issues: vec![issue(Severity::Minor)],
            summary: String::new(),
            tokens_used: None,
            should_block: true, // stale value from the judge
            dropped_findings: 0,
        };
        shape(&mut resp, &config("warn", false), &opts(Severity::Major));
        assert!(resp.issues.is_empty());
        assert!(!resp.should_block);
    }

    #[test]
    fn gate_is_skipped_when_disabled_by_config_or_option() {
        let mut issues = vec![issue(Severity::Critical)];
        let shaped = shape_issues(&mut issues, &config("warn", false), &opts(Severity::Info));
        assert!(shaped.gate.is_none());
        assert_eq!(shaped.gate_label(), "disabled");

        let o = ShapeOptions {
            gate: false,
            ..opts(Severity::Info)
        };
        let shaped = shape_issues(&mut issues, &config("warn", true), &o);
        assert!(shaped.gate.is_none());
    }

    #[test]
    fn gate_runs_on_the_filtered_list() {
        // A critical finding fails the default gate (max_critical = 0)...
        let mut issues = vec![issue(Severity::Critical)];
        let shaped = shape_issues(&mut issues, &config("warn", true), &opts(Severity::Info));
        assert!(shaped.gate_failed());
        assert_eq!(shaped.gate_label(), "failed");
        assert_eq!(shaped.exit_code, EXIT_BLOCKED);

        // ...and a clean list passes it.
        let mut none: Vec<ReviewIssue> = vec![];
        let shaped = shape_issues(&mut none, &config("warn", true), &opts(Severity::Info));
        assert!(!shaped.gate_failed());
        assert_eq!(shaped.gate_label(), "passed");
        assert_eq!(shaped.exit_code, EXIT_OK);
    }

    // ─── exit code ───

    #[test]
    fn exit_code_zero_when_hook_mode_not_block() {
        assert_eq!(exit_code(None, false, true, true, "warn"), EXIT_OK);
        assert_eq!(exit_code(None, false, true, true, ""), EXIT_OK);
    }

    #[test]
    fn exit_code_blocked_when_should_block_and_hook_blocks() {
        assert_eq!(exit_code(None, false, true, true, "block"), EXIT_BLOCKED);
    }

    #[test]
    fn exit_code_ci_blocks_on_any_finding() {
        assert_eq!(exit_code(None, true, false, false, "block"), EXIT_OK);
        assert_eq!(exit_code(None, true, true, false, "block"), EXIT_BLOCKED);
    }

    #[test]
    fn exit_code_gate_fail_overrides_everything() {
        assert_eq!(
            exit_code(Some(GateStatus::Fail), false, false, false, "block"),
            EXIT_BLOCKED
        );
        assert_eq!(
            exit_code(Some(GateStatus::Pass), false, false, false, "block"),
            EXIT_OK
        );
    }
}
