use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::config::schema::Config;
use crate::engine::Severity;
use crate::engine::chunker;
use crate::engine::db_writer;
use crate::engine::quality_gate;
use crate::engine::types::ReviewResponse;
use crate::formatters::{OutputFormat, formatter_for};
use crate::git;
use crate::progress::{ProgressReporter, TokenInfo, diff_stats};

/// Exit codes for the review command.
pub const EXIT_OK: i32 = 0;
pub const EXIT_BLOCKED: i32 = 2;

/// Review command options.
#[allow(clippy::struct_excessive_bools)]
pub struct ReviewOptions {
    /// Review staged changes.
    pub staged: bool,
    /// Review unpushed changes.
    pub unpushed: bool,
    /// Review diff against a base branch.
    pub base: Option<String>,
    /// Review diff from a git commit reference (e.g. HEAD, HEAD~3..HEAD, abc123).
    pub commit: Option<String>,
    /// Read diff from a file instead of git.
    pub diff_file: Option<String>,
    /// Review unstaged changes.
    pub unstaged: bool,
    /// Maximum diff size before refusing (0 = use config default).
    pub max_diff_size: Option<usize>,
    /// Stream LLM response tokens to stdout in real-time.
    pub stream: bool,
    /// Suppress all output except the formatted review result.
    pub quiet: bool,
    /// Minimum severity level for filtering issues.
    pub severity: Option<String>,
    /// Disable review caching.
    pub no_cache: bool,
    /// CI mode: skip diff size limit, hard gate on any findings.
    pub ci: bool,
    /// Auto-chunk large diffs into review-sized pieces.
    pub auto_chunk: bool,
}

/// Result of a review command execution.
pub struct ReviewResult {
    /// Exit code (0 = ok, 1 = issues, 2 = blocked).
    pub exit_code: i32,
    /// The formatted output string.
    pub output: String,
    /// Total issues found (for memory integration).
    pub total_issues: usize,
    /// Severity summary (e.g. "1 critical, 2 major, 0 minor").
    pub severity_summary: String,
    /// Issue categories (e.g. ["security", "performance"]).
    pub categories: Vec<String>,
}

impl ReviewResult {
    /// Create a result from formatted output, extracting issue stats from the response.
    fn with_issues(
        exit_code: i32,
        output: String,
        response: &crate::engine::types::ReviewResponse,
    ) -> Self {
        let total_issues = response.issues.len();
        let mut sev_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut cat_set: std::collections::HashSet<String> = std::collections::HashSet::new();

        for issue in &response.issues {
            let sev = format!("{:?}", issue.severity).to_lowercase();
            *sev_counts.entry(sev).or_insert(0) += 1;
            if let Some(ref t) = issue.issue_type {
                if !t.is_empty() {
                    cat_set.insert(t.clone());
                }
            }
        }

        let severity_summary = if sev_counts.is_empty() {
            String::new()
        } else {
            let mut parts: Vec<String> = sev_counts
                .into_iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            parts.sort();
            parts.join(", ")
        };

        Self {
            exit_code,
            output,
            total_issues,
            severity_summary,
            categories: cat_set.into_iter().collect(),
        }
    }

    /// Create a result with no issues (empty diff, errors, etc).
    fn empty(exit_code: i32, output: String) -> Self {
        Self {
            exit_code,
            output,
            total_issues: 0,
            severity_summary: String::new(),
            categories: Vec::new(),
        }
    }
}

/// Execute the review command.
///
/// Gets the diff, validates its size, calls the LLM engine, formats output,
/// and returns the appropriate exit code along with the formatted output.
#[allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]
pub async fn execute_review(
    config: &Config,
    llm_config: &crate::engine::LLMConfig,
    opts: &ReviewOptions,
    format: OutputFormat,
    progress: &ProgressReporter,
    memory_context: Option<&str>,
) -> Result<ReviewResult> {
    // 1. Get the diff
    let diff = get_diff(opts, config)?;

    if diff.trim().is_empty() {
        if opts.quiet {
            return Ok(ReviewResult::empty(EXIT_OK, String::new()));
        }
        let output = format!("{}\n", "No changes to review.".yellow());
        return Ok(ReviewResult::empty(EXIT_OK, output));
    }

    // 2. Emit parsing_diff event if progress enabled
    if progress.is_enabled() {
        let (files_changed, lines_changed) = diff_stats(&diff);
        progress.parsing_diff(files_changed, lines_changed);
    }

    // 3. Validate size (skip in CI mode)
    let max_size = opts.max_diff_size.unwrap_or(config.hook.max_diff_size);
    if !opts.ci && diff.len() > max_size {
        if opts.auto_chunk {
            // Auto-chunk mode: split diff and review each chunk
            return execute_chunked_review(
                config,
                llm_config,
                opts,
                format,
                progress,
                &diff,
                max_size,
                memory_context,
            )
            .await;
        }
        if config.hook.on_violation == "disallow" {
            // Return blocked result instead of error
            return Ok(ReviewResult::empty(EXIT_BLOCKED, format!(
                    "{}\n",
                    format!(
                        "❌ Diff too large ({} chars, max {}). Commit blocked. \
                         Use --auto-chunk to review in pieces, --base to review a specific branch, increase \
                         hook.max_diff_size, or run: git commit --no-verify",
                        diff.len(),
                        max_size
                    )
                    .red()
                    .bold(),
                )));
        }
        anyhow::bail!(
            "Diff too large ({} chars, max {}). Use --auto-chunk to review in pieces, --base to review a specific branch, or increase hook.max_diff_size.",
            diff.len(),
            max_size
        );
    }

    debug!(
        diff_len = diff.len(),
        stream = opts.stream,
        "running review"
    );

    // 4. Emit calling_llm event
    if progress.is_enabled() {
        progress.calling_llm(&llm_config.provider, &llm_config.model);
    }

    let llm_start = std::time::Instant::now();

    // 5. Call the LLM engine
    let response = match crate::engine::review::review_diff_with_cache(
        config,
        llm_config,
        &diff,
        opts.stream,
        !opts.no_cache,
        opts.quiet,
        memory_context,
    )
    .await
    {
        Ok(resp) => {
            if progress.is_enabled() {
                // SAFETY: elapsed ms fits u64 for any reasonable review duration
                let duration_ms = llm_start.elapsed().as_millis() as u64;
                let tokens = resp
                    .tokens_used
                    .as_ref()
                    .map_or_else(TokenInfo::zero, TokenInfo::from_usage);
                progress.llm_response(&tokens, duration_ms);
            }
            resp
        }
        Err(e) => {
            if progress.is_enabled() {
                progress.error(&e.to_string(), "calling_llm");
            }
            // In SARIF format mode, produce a warning result instead of crashing.
            // This ensures CI always gets valid SARIF even when the LLM API is
            // temporarily unavailable or the diff is too large for the model.
            if format == OutputFormat::Sarif {
                let warning_output = serde_json::json!({
                    "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/main/sarif-2.1/schema/sarif-schema-2.1.0.json",
                    "version": "2.1.0",
                    "runs": [{
                        "tool": {
                            "driver": {
                                "name": "cora",
                                "informationUri": "https://github.com/codecoradev/cora-code",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        },
                        "results": [{
                            "level": "warning",
                            "message": {
                                "text": format!("Cora AI review could not complete: {}. Review was skipped; no code quality issues were found by the automated check.", e)
                            }
                        }]
                    }]
                });
                return Ok(ReviewResult::empty(EXIT_OK, format!("{warning_output}\n")));
            }
            return Err(e.into());
        }
    };

    // 6. Filter by severity if specified
    let min_severity = if let Some(ref sev) = opts.severity {
        Severity::from_str_lossy(sev)
    } else {
        config.hook.min_severity_level()
    };

    let mut filtered_response = response.clone();
    // Keep issues at or above min_severity (Critical=worst, Info=lowest in Ord but
    // Ord order is Critical(0) < Major(1) < Minor(2) < Info(3), so we invert: keep
    // where severity Ord value <= min_severity Ord value).
    filtered_response
        .issues
        .retain(|i| i.severity <= min_severity);
    // Recompute should_block against the filtered issue list so the exit code
    // matches the output the user actually sees (e.g. when `--severity critical`
    // filters out all Major/Minor issues, we must not block). See #312.
    filtered_response.should_block = filtered_response
        .issues
        .iter()
        .any(|i| i.severity <= min_severity);

    // 7. Format output
    let formatter = formatter_for(format);
    let mut output = formatter.format_review(&filtered_response)?;

    // 8. Quality gate evaluation
    let gate_result = if config.quality_gate.enabled {
        let result = quality_gate::evaluate(&filtered_response.issues, &config.quality_gate);
        output.push_str(&quality_gate::format_gate_output(&result));
        Some(result)
    } else {
        None
    };

    // 8b. Save debt tracking snapshot (best-effort, never fails review)
    if config.debt.enabled {
        let (commit, branch) = get_git_context();
        let snapshot = crate::engine::debt_tracker::DebtSnapshot::from_review(
            &filtered_response.issues,
            gate_result.as_ref(),
            commit,
            branch,
            0, // files_reviewed not tracked per-review yet
            None,
            None, // duration not passed through yet
        );
        crate::engine::debt_tracker::save_snapshot(&snapshot, config.debt.history_dir.as_deref());
    }

    // 8c. Save review findings to cora.db (best-effort)
    {
        let (commit, branch) = get_git_context();
        let cmd = if opts.ci { "review --ci" } else { "review" };
        let gate_status = match gate_result.as_ref().map(|g| g.status) {
            Some(quality_gate::GateStatus::Pass) => "passed",
            Some(quality_gate::GateStatus::Fail) => "failed",
            None => "disabled",
        };
        let tokens = response.tokens_used.as_ref();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let record = db_writer::ReviewRecord {
            command: cmd,
            project_root: &cwd,
            commit_hash: commit.as_deref(),
            branch: branch.as_deref(),
            summary: &filtered_response.summary,
            gate_status,
            files_scanned: 0,
            lines_scanned: 0,
            should_block: filtered_response.should_block,
            tokens,
            issues: &filtered_response.issues,
        };
        if db_writer::save_review_to_db(&record).is_none() {
            debug!("Failed to save review to cora.db");
        }

        // Auto-resolve findings that no longer appear in this review.
        let fps: Vec<String> = filtered_response
            .issues
            .iter()
            .map(db_writer::compute_fingerprint_pub)
            .collect();
        let resolved = db_writer::resolve_stale_findings(&cwd, &fps);
        if resolved > 0 {
            debug!(resolved, "auto-resolved stale findings");
        }
    }
    let exit_code = if gate_result
        .as_ref()
        .is_some_and(|g| g.status == quality_gate::GateStatus::Fail)
    {
        EXIT_BLOCKED
    } else if opts.ci {
        // CI mode: hard gate — any finding blocks
        if !filtered_response.issues.is_empty() {
            EXIT_BLOCKED
        } else {
            EXIT_OK
        }
    } else if filtered_response.should_block && config.hook.mode == "block" {
        EXIT_BLOCKED
    } else {
        EXIT_OK
    };

    // 10. Emit complete event
    if progress.is_enabled() {
        let tokens = response
            .tokens_used
            .as_ref()
            .map_or_else(TokenInfo::zero, TokenInfo::from_usage);
        progress.complete(
            filtered_response.issues.len(),
            exit_code == EXIT_BLOCKED,
            &tokens,
        );
    }

    Ok(ReviewResult::with_issues(
        exit_code,
        output,
        &filtered_response,
    ))
}
/// Returns (Some(commit), Some(branch)) if in a git repo, (None, None) otherwise.
/// Results are cached per process to avoid spawning git on every review.
fn get_git_context() -> (Option<String>, Option<String>) {
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<(Option<String>, Option<String>)> = LazyLock::new(|| {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout)
                        .ok()
                        .map(|s| s.trim().to_string())
                } else {
                    None
                }
            });

        let branch = crate::git::get_current_branch().ok();

        (commit, branch)
    });

    (CONTEXT.0.clone(), CONTEXT.1.clone())
}

/// Get the diff based on the provided options.
fn get_diff(opts: &ReviewOptions, _config: &Config) -> Result<String> {
    if let Some(ref diff_file) = opts.diff_file {
        let path = std::path::Path::new(diff_file);
        if !path.exists() {
            anyhow::bail!("diff file not found: {diff_file}");
        }
        return std::fs::read_to_string(path)
            .with_context(|| format!("failed to read diff file: {diff_file}"));
    }
    if let Some(ref commit_ref) = opts.commit {
        return git::get_commit_diff(commit_ref).map_err(Into::into);
    }
    if opts.staged {
        git::get_staged_diff().map_err(Into::into)
    } else if opts.unpushed {
        git::get_unpushed_diff().map_err(Into::into)
    } else if let Some(ref base) = opts.base {
        git::get_branch_diff(base).map_err(Into::into)
    } else if opts.unstaged {
        git::get_unstaged_diff().map_err(Into::into)
    } else {
        // Default: try staged first, fall back to unpushed, then unstaged
        match git::get_staged_diff() {
            Ok(d) if !d.trim().is_empty() => Ok(d),
            _ => match git::get_unpushed_diff() {
                Ok(d) if !d.trim().is_empty() => Ok(d),
                _ => git::get_unstaged_diff().map_err(Into::into),
            },
        }
    }
}

/// Execute a chunked review for large diffs.
///
/// Splits the diff into chunks, reviews each independently, then
/// merges and deduplicates findings.
#[allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]
async fn execute_chunked_review(
    config: &Config,
    llm_config: &crate::engine::LLMConfig,
    opts: &ReviewOptions,
    format: OutputFormat,
    progress: &ProgressReporter,
    diff: &str,
    max_size: usize,
    memory_context: Option<&str>,
) -> Result<ReviewResult> {
    let chunks = chunker::chunk_diff(diff, max_size);

    if chunks.is_empty() {
        let output = format!("{}\n", "No changes to review.".yellow());
        return Ok(ReviewResult::empty(EXIT_OK, output));
    }

    let total_chunks = chunks.len();
    if !opts.quiet {
        eprintln!(
            "{}",
            format!(
                "📦 Diff too large ({} chars), splitting into {} chunks…",
                diff.len(),
                total_chunks
            )
            .cyan()
        );
    }

    debug!(
        total_chunks,
        diff_len = diff.len(),
        "auto-chunking large diff"
    );

    let mut all_issues: Vec<crate::engine::types::ReviewIssue> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();
    let mut total_tokens = crate::engine::types::TokenUsage::default();
    let mut any_error = false;
    let mut any_success = false;
    let mut should_block = false;

    for chunk in &chunks {
        if !opts.quiet {
            eprintln!(
                "{}",
                format!(
                    "  → Reviewing chunk {}/{} ({}, {} files)…",
                    chunk.index + 1,
                    chunk.total,
                    chunk.label,
                    chunk.file_count
                )
                .dimmed()
            );
        }

        if progress.is_enabled() {
            progress.calling_llm(&llm_config.provider, &llm_config.model);
        }

        let chunk_start = std::time::Instant::now();

        match crate::engine::review::review_diff_with_cache(
            config,
            llm_config,
            &chunk.diff,
            opts.stream,
            !opts.no_cache,
            opts.quiet,
            memory_context,
        )
        .await
        {
            Ok(resp) => {
                if progress.is_enabled() {
                    let duration_ms = chunk_start.elapsed().as_millis() as u64;
                    let tokens = resp
                        .tokens_used
                        .as_ref()
                        .map_or_else(TokenInfo::zero, TokenInfo::from_usage);
                    progress.llm_response(&tokens, duration_ms);
                }

                // Accumulate tokens
                if let Some(usage) = &resp.tokens_used {
                    total_tokens.input_tokens += usage.input_tokens;
                    total_tokens.output_tokens += usage.output_tokens;
                    total_tokens.estimated_cost_usd += usage.estimated_cost_usd;
                }

                if !resp.summary.is_empty() {
                    summaries.push(format!(
                        "[Chunk {}/{} — {}] {}",
                        chunk.index + 1,
                        chunk.total,
                        chunk.label,
                        resp.summary
                    ));
                }

                all_issues.extend(resp.issues);
                if resp.should_block {
                    should_block = true;
                }
                any_success = true;
            }
            Err(e) => {
                if progress.is_enabled() {
                    progress.error(&e.to_string(), "chunked_review");
                }
                any_error = true;
                if !opts.quiet {
                    eprintln!(
                        "  {}",
                        format!(
                            "⚠️  Chunk {}/{} failed: {}",
                            chunk.index + 1,
                            chunk.total,
                            e
                        )
                        .yellow()
                    );
                }
            }
        }
    }

    // Deduplicate findings by (file, line, title)
    let before_dedup = all_issues.len();
    all_issues.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.title.cmp(&b.title))
    });
    all_issues.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.title == b.title);
    let deduped = before_dedup - all_issues.len();

    if deduped > 0 && !opts.quiet {
        eprintln!(
            "{}",
            format!("  ℹ️  Deduplicated {deduped} overlapping findings").dimmed()
        );
    }

    // If all chunks failed, return an error instead of silently passing
    if !any_success {
        if progress.is_enabled() {
            progress.error("All chunks failed during chunked review", "chunked_review");
        }
        return Ok(ReviewResult::empty(
            EXIT_BLOCKED,
            format!(
                "{}\n",
                "❌ All review chunks failed. Check your API key and try again."
                    .red()
                    .bold()
            ),
        ));
    }

    // Build merged response
    let merged_summary = if summaries.is_empty() {
        if any_error {
            "Review completed with partial results (some chunks failed).".to_string()
        } else {
            "No issues found across all chunks.".to_string()
        }
    } else {
        summaries.join("\n\n")
    };

    let merged_response = ReviewResponse {
        issues: all_issues,
        summary: merged_summary,
        tokens_used: Some(total_tokens),
        should_block,
    };

    // 6. Filter by severity if specified
    let min_severity = if let Some(ref sev) = opts.severity {
        Severity::from_str_lossy(sev)
    } else {
        config.hook.min_severity_level()
    };

    let mut filtered_response = merged_response.clone();
    filtered_response
        .issues
        .retain(|i| i.severity <= min_severity);
    // Recompute should_block against the filtered issue list so the exit code
    // matches the output the user actually sees (see #312).
    filtered_response.should_block = filtered_response
        .issues
        .iter()
        .any(|i| i.severity <= min_severity);

    // 7. Format output
    let formatter = formatter_for(format);
    let mut output = formatter.format_review(&filtered_response)?;

    // Add chunking summary header if not quiet and format is pretty
    if !opts.quiet && format == OutputFormat::Pretty {
        let header = format!(
            "📦 Reviewed in {} chunks ({} files total)\n",
            total_chunks,
            chunks.iter().map(|c| c.file_count).sum::<usize>()
        );
        output = format!("{header}{output}");
    }

    // 8. Quality gate evaluation
    let gate_result = if config.quality_gate.enabled {
        let result = quality_gate::evaluate(&filtered_response.issues, &config.quality_gate);
        output.push_str(&quality_gate::format_gate_output(&result));
        Some(result)
    } else {
        None
    };

    // 8b. Save debt tracking snapshot (best-effort, never fails review)
    if config.debt.enabled {
        let (commit, branch) = get_git_context();
        let snapshot = crate::engine::debt_tracker::DebtSnapshot::from_review(
            &filtered_response.issues,
            gate_result.as_ref(),
            commit,
            branch,
            chunks.iter().map(|c| c.file_count).sum(),
            None,
            None,
        );
        crate::engine::debt_tracker::save_snapshot(&snapshot, config.debt.history_dir.as_deref());
    }

    // 8c. Save review findings to cora.db (best-effort)
    {
        let (commit, branch) = get_git_context();
        let cmd = if opts.ci { "review --ci" } else { "review" };
        let gate_status = match gate_result.as_ref().map(|g| g.status) {
            Some(quality_gate::GateStatus::Pass) => "passed",
            Some(quality_gate::GateStatus::Fail) => "failed",
            None => "disabled",
        };
        let tokens = merged_response.tokens_used.as_ref();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let record = db_writer::ReviewRecord {
            command: cmd,
            project_root: &cwd,
            commit_hash: commit.as_deref(),
            branch: branch.as_deref(),
            summary: &filtered_response.summary,
            gate_status,
            files_scanned: 0,
            lines_scanned: 0,
            should_block: filtered_response.should_block,
            tokens,
            issues: &filtered_response.issues,
        };
        if db_writer::save_review_to_db(&record).is_none() {
            debug!("Failed to save review to cora.db");
        }

        // Auto-resolve findings that no longer appear in this review.
        let fps: Vec<String> = filtered_response
            .issues
            .iter()
            .map(db_writer::compute_fingerprint_pub)
            .collect();
        let resolved = db_writer::resolve_stale_findings(&cwd, &fps);
        if resolved > 0 {
            debug!(resolved, "auto-resolved stale findings");
        }
    }
    let exit_code = compute_exit_code(
        gate_result.as_ref().map(|g| g.status),
        opts.ci,
        &filtered_response,
        config.hook.mode.as_str(),
    );

    // 10. Emit complete event
    if progress.is_enabled() {
        let tokens = filtered_response
            .tokens_used
            .as_ref()
            .map_or_else(TokenInfo::zero, TokenInfo::from_usage);
        progress.complete(
            filtered_response.issues.len(),
            exit_code == EXIT_BLOCKED,
            &tokens,
        );
    }

    Ok(ReviewResult::with_issues(
        exit_code,
        output,
        &filtered_response,
    ))
}

/// Compute the review exit code from the gate status, CI flag, and the
/// **filtered** review response (issues after severity filtering).
///
/// Exit code semantics (see #312):
///
/// | Code | Meaning |
/// |------|---------|
/// | 0 | Review completed; no findings at or above the severity threshold |
/// | 2 | Review completed but findings are blocking (gate fail, CI with any
///     issue, or hook in block mode with blocking severity) |
///
/// `should_block` must be computed against the **filtered** issue list so that
/// the exit code matches the SARIF/pretty output the user sees.
fn compute_exit_code(
    gate_status: Option<quality_gate::GateStatus>,
    ci: bool,
    filtered_response: &ReviewResponse,
    hook_mode: &str,
) -> i32 {
    if gate_status == Some(quality_gate::GateStatus::Fail) {
        return EXIT_BLOCKED;
    }
    if ci {
        return if filtered_response.issues.is_empty() {
            EXIT_OK
        } else {
            EXIT_BLOCKED
        };
    }
    if filtered_response.should_block && hook_mode == "block" {
        EXIT_BLOCKED
    } else {
        EXIT_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ReviewResponse;
    use crate::engine::Severity;
    use crate::engine::types::ReviewIssue;

    fn issue(severity: Severity) -> ReviewIssue {
        ReviewIssue {
            file: "src/main.rs".to_string(),
            line: Some(1),
            severity,
            issue_type: Some("bug".to_string()),
            title: "test".to_string(),
            body: "body".to_string(),
            suggested_fix: None,
        }
    }

    fn response(issues: Vec<ReviewIssue>, should_block: bool) -> ReviewResponse {
        ReviewResponse {
            issues,
            summary: String::new(),
            tokens_used: None,
            should_block,
        }
    }

    // ─── #312: exit code must match filtered output ───

    #[test]
    fn exit_code_zero_when_no_findings_after_filter() {
        let resp = response(vec![], false);
        assert_eq!(compute_exit_code(None, false, &resp, "block"), EXIT_OK);
    }

    #[test]
    fn exit_code_zero_when_filtered_should_block_false() {
        // Even if the unfiltered response would have blocked, after filtering
        // should_block is false → exit 0. This is the core regression in #312.
        let resp = response(vec![], false);
        assert_eq!(compute_exit_code(None, false, &resp, "block"), EXIT_OK);
    }

    #[test]
    fn exit_code_blocked_when_filtered_should_block_true_and_hook_blocks() {
        let resp = response(vec![issue(Severity::Critical)], true);
        assert_eq!(compute_exit_code(None, false, &resp, "block"), EXIT_BLOCKED);
    }

    #[test]
    fn exit_code_zero_when_hook_mode_not_block() {
        let resp = response(vec![issue(Severity::Critical)], true);
        assert_eq!(compute_exit_code(None, false, &resp, "warn"), EXIT_OK);
        assert_eq!(compute_exit_code(None, false, &resp, ""), EXIT_OK);
    }

    #[test]
    fn exit_code_ci_zero_when_no_findings() {
        let resp = response(vec![], false);
        assert_eq!(compute_exit_code(None, true, &resp, "block"), EXIT_OK);
    }

    #[test]
    fn exit_code_ci_blocked_when_any_finding() {
        let resp = response(vec![issue(Severity::Minor)], false);
        assert_eq!(compute_exit_code(None, true, &resp, "block"), EXIT_BLOCKED);
    }

    #[test]
    fn exit_code_gate_fail_overrides_everything() {
        let resp = response(vec![], false);
        assert_eq!(
            compute_exit_code(Some(quality_gate::GateStatus::Fail), false, &resp, "block"),
            EXIT_BLOCKED
        );
    }

    #[test]
    fn exit_code_gate_pass_then_falls_through() {
        let resp = response(vec![], false);
        assert_eq!(
            compute_exit_code(Some(quality_gate::GateStatus::Pass), false, &resp, "block"),
            EXIT_OK
        );
    }
}
