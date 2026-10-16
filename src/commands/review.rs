use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::config::schema::Config;
use crate::engine::Severity;
use crate::engine::chunker;
use crate::engine::finalize::{self, HistoryEntry, ShapeOptions};
use crate::engine::judge::JudgeChoice;
use crate::engine::quality_gate;
use crate::engine::types::ReviewResponse;
use crate::formatters::{OutputFormat, formatter_for};
use crate::git;
use crate::progress::{ProgressReporter, TokenInfo, diff_stats};

/// Exit codes for the review command.
pub use crate::engine::types::{EXIT_BLOCKED, EXIT_OK};

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
    judge: &JudgeChoice,
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
                judge,
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
        if let JudgeChoice::Llm(llm_config) = judge {
            progress.calling_llm(&llm_config.provider, &llm_config.model);
        }
    }

    let llm_start = std::time::Instant::now();

    // 5. Call the LLM engine
    let response = match crate::engine::review::review_diff_with_judge(
        config,
        judge,
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

    warn_dropped(response.dropped_findings, config, opts.quiet);

    // 6. Filter by severity, decide gate / blocking / exit code
    let min_severity = if let Some(ref sev) = opts.severity {
        Severity::from_str_lossy(sev)
    } else {
        config.hook.min_severity_level()
    };

    let mut filtered_response = response.clone();
    let shaped = finalize::shape(
        &mut filtered_response,
        config,
        &ShapeOptions {
            min_severity,
            filter: true,
            gate: true,
            ci: opts.ci,
        },
    );

    // 7. Format output
    let formatter = formatter_for(format);
    let mut output = formatter.format_review(&filtered_response)?;

    // 8. Quality gate output, debt snapshot, review history (best-effort)
    if let Some(gate) = &shaped.gate {
        output.push_str(&quality_gate::format_gate_output(gate));
    }
    finalize::save_debt(config, &filtered_response.issues, shaped.gate.as_ref(), 0);
    finalize::save_history(
        &HistoryEntry {
            command: review_command(opts),
            summary: &filtered_response.summary,
            files_scanned: 0,
            lines_scanned: 0,
            tokens: response.tokens_used.as_ref(),
        },
        &filtered_response.issues,
        &shaped,
    );
    let exit_code = shaped.exit_code;

    // Leave a verifiable trace for the commit gate (#674): a staged diff that
    // was really reviewed (not a failed LLM call) may be committed by the
    // pre-commit hook without a second review.
    // Only a threshold at least as strict as the hook's (major) may stand in for the hook's review.
    if opts.staged
        && min_severity >= Severity::Major
        && !response.summary.starts_with("LLM review failed")
    {
        let judged_by = match judge {
            JudgeChoice::Llm(_) => "llm",
            JudgeChoice::Deterministic => "deterministic",
        };
        crate::engine::commit_gate::record_for_diff(
            &diff,
            judged_by,
            shaped.should_block || exit_code == EXIT_BLOCKED,
            filtered_response.issues.len(),
        );
    }

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
/// Command label stored in the review history.
fn review_command(opts: &ReviewOptions) -> &'static str {
    if opts.ci { "review --ci" } else { "review" }
}

/// Merged summary for chunked review reports (#556).
///
/// Per-chunk summaries win when present. Otherwise the fallback must
/// reflect reality: chunks can return issues with an EMPTY summary
/// string, so the no-findings message must only be printed when
/// `issue_count == 0`. The old fallback keyed only off `summaries` /
/// `any_error` and printed "No issues found across all chunks." even
/// when issues were found (live: gaira PR #108, run #1388 verdict).
fn merged_chunk_summary(summaries: &[String], issue_count: usize, any_error: bool) -> String {
    if !summaries.is_empty() {
        return summaries.join("\n\n");
    }
    if issue_count > 0 {
        if any_error {
            return format!(
                "{issue_count} issue(s) found across chunks; review completed with partial results (some chunks failed)."
            );
        }
        return format!("{issue_count} issue(s) found across chunks.");
    }
    if any_error {
        "Review completed with partial results (some chunks failed).".to_string()
    } else {
        "No issues found across all chunks.".to_string()
    }
}

fn get_diff(opts: &ReviewOptions, _config: &Config) -> Result<String> {
    // Get the diff based on the provided options.
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
    judge: &JudgeChoice,
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
    let mut dropped_findings = 0usize;

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
            if let JudgeChoice::Llm(llm_config) = judge {
                progress.calling_llm(&llm_config.provider, &llm_config.model);
            }
        }

        let chunk_start = std::time::Instant::now();

        match crate::engine::review::review_diff_with_judge(
            config,
            judge,
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

                dropped_findings += resp.dropped_findings;
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
    let merged_summary = merged_chunk_summary(&summaries, all_issues.len(), any_error);

    warn_dropped(dropped_findings, config, opts.quiet);

    let merged_response = ReviewResponse {
        issues: all_issues,
        summary: merged_summary,
        tokens_used: Some(total_tokens),
        should_block,
        dropped_findings,
    };

    // 6. Filter by severity, decide gate / blocking / exit code
    let min_severity = if let Some(ref sev) = opts.severity {
        Severity::from_str_lossy(sev)
    } else {
        config.hook.min_severity_level()
    };

    let mut filtered_response = merged_response.clone();
    let shaped = finalize::shape(
        &mut filtered_response,
        config,
        &ShapeOptions {
            min_severity,
            filter: true,
            gate: true,
            ci: opts.ci,
        },
    );

    // 7. Format output
    let formatter = formatter_for(format);
    let mut output = formatter.format_review(&filtered_response)?;

    // Add chunking summary header if not quiet and format is pretty
    let files_total: usize = chunks.iter().map(|c| c.file_count).sum();
    if !opts.quiet && format == OutputFormat::Pretty {
        let header = format!("📦 Reviewed in {total_chunks} chunks ({files_total} files total)\n");
        output = format!("{header}{output}");
    }

    // 8. Quality gate output, debt snapshot, review history (best-effort)
    if let Some(gate) = &shaped.gate {
        output.push_str(&quality_gate::format_gate_output(gate));
    }
    finalize::save_debt(
        config,
        &filtered_response.issues,
        shaped.gate.as_ref(),
        files_total,
    );
    finalize::save_history(
        &HistoryEntry {
            command: review_command(opts),
            summary: &filtered_response.summary,
            files_scanned: 0,
            lines_scanned: 0,
            tokens: merged_response.tokens_used.as_ref(),
        },
        &filtered_response.issues,
        &shaped,
    );
    let exit_code = shaped.exit_code;

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

/// Print the one-line "findings not shown" warning to stderr (#624).
fn warn_dropped(dropped: usize, config: &Config, quiet: bool) {
    if quiet {
        return;
    }
    if let Some(w) = crate::engine::postprocess::dropped_warning(dropped, config) {
        eprintln!("{}", w.yellow());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── #556: chunked summary fallback must reflect actual findings ───

    #[test]
    fn chunk_summary_issues_without_summaries_are_reported() {
        // Live regression: gaira PR #108 run #1388 — chunks returned issues
        // with empty summaries; old code printed "No issues found across
        // all chunks." under a "Found 1 issue" header.
        let s = merged_chunk_summary(&[], 1, false);
        assert_eq!(s, "1 issue(s) found across chunks.");
        assert!(!s.contains("No issues found"));
    }

    #[test]
    fn chunk_summary_no_findings_message_only_when_clean() {
        let s = merged_chunk_summary(&[], 0, false);
        assert_eq!(s, "No issues found across all chunks.");
    }

    #[test]
    fn chunk_summary_partial_error_with_issues_mentions_both() {
        let s = merged_chunk_summary(&[], 2, true);
        assert!(s.starts_with("2 issue(s) found across chunks;"));
        assert!(s.contains("partial results"));
    }

    #[test]
    fn chunk_summary_partial_error_without_issues_stays_honest() {
        let s = merged_chunk_summary(&[], 0, true);
        assert_eq!(
            s,
            "Review completed with partial results (some chunks failed)."
        );
    }

    #[test]
    fn chunk_summary_per_chunk_summaries_win() {
        let summaries = vec![
            "[Chunk 1/2 — a] ok".to_string(),
            "[Chunk 2/2 — b] fine".to_string(),
        ];
        let s = merged_chunk_summary(&summaries, 0, false);
        assert_eq!(s, "[Chunk 1/2 — a] ok\n\n[Chunk 2/2 — b] fine");
    }
}
