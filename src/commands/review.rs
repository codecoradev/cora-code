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

use crate::engine::path_match::PathMatcher;
use crate::engine::types::EXIT_ERROR;
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
    // 1. Get the diff, minus the files `review.exclude` removes (#720)
    let full_diff = get_diff(opts, config)?;
    // Dependencies, build output, minified files and lockfiles are out of scope
    // by default (`ignore.defaults`): nothing about them reaches the scanners or
    // the model. Say so once, so a missing file in the report is not a mystery.
    // `full_diff` stays what the commit gate fingerprints: the verdict is
    // recorded for the whole staged diff, as the hook looks it up (#767).
    let (diff, skipped_by_default) =
        crate::engine::review::drop_builtin_ignored(config, &full_diff);
    if !skipped_by_default.is_empty() && !opts.quiet {
        eprintln!(
            "ℹ {} file(s) skipped by the built-in ignores (dependencies, build output, lockfiles; \
             `ignore.defaults: false` turns them off): {}",
            skipped_by_default.len(),
            skipped_by_default
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
                + if skipped_by_default.len() > 5 {
                    ", …"
                } else {
                    ""
                }
        );
    }
    let matcher = PathMatcher::new(&config.review.exclude);
    let (diff, excluded) = if matcher.is_empty() {
        (diff, Vec::new())
    } else {
        let out = chunker::exclude_paths(&diff, &matcher);
        (out.diff, out.excluded)
    };

    if diff.trim().is_empty() {
        // Everything was excluded: nothing is left to judge, and the commit gate
        // must not call the staged diff unreviewed.
        if opts.staged && !full_diff.trim().is_empty() {
            crate::engine::commit_gate::record_for_diff(&full_diff, "deterministic", false, 0);
        }
        if opts.quiet {
            return Ok(ReviewResult::empty(EXIT_OK, String::new()));
        }
        let mut output = format!("{}\n", "No changes to review.".yellow());
        if !excluded.is_empty() {
            output.push_str(&coverage_note(&excluded, &[]));
        }
        return Ok(ReviewResult::empty(EXIT_OK, output));
    }

    // 2. Emit parsing_diff event if progress enabled
    if progress.is_enabled() {
        let (files_changed, lines_changed) = diff_stats(&diff);
        progress.parsing_diff(files_changed, lines_changed);
    }

    // 3. Size policy. A diff over the request budget is split and reviewed in
    //    chunks, in CI mode too (#720); `max_diff_size` stays the hard limit
    //    for runs that cannot chunk.
    let max_size = opts.max_diff_size.unwrap_or(config.hook.max_diff_size);
    let budget = config.review.max_chunk_chars.min(max_size).max(1);
    if opts.auto_chunk && diff.len() > budget {
        return execute_chunked_review(
            config,
            judge,
            opts,
            format,
            progress,
            &diff,
            budget,
            &excluded,
            memory_context,
        )
        .await;
    }
    if !opts.ci && diff.len() > max_size {
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
    let first_try = crate::engine::review::review_diff_with_judge(
        config,
        judge,
        &diff,
        opts.stream,
        !opts.no_cache,
        opts.quiet,
        memory_context,
    )
    .await;

    // The model could not review the whole diff in one request (empty reply at
    // finish_reason=length, gateway timeout, unparseable reply): do not just
    // fail, review it in smaller pieces instead (#718).
    let judge_failed = match &first_try {
        Err(_) => true,
        Ok(resp) => resp.judge_failure.is_some(),
    };
    if judge_failed && opts.auto_chunk && retry_budget(&diff).is_some() {
        if !opts.quiet {
            eprintln!(
                "{}",
                "↻ The model could not review the diff in one request; retrying in smaller chunks…"
                    .yellow()
            );
        }
        let budget = retry_budget(&diff).unwrap_or(budget);
        return execute_chunked_review(
            config,
            judge,
            opts,
            format,
            progress,
            &diff,
            budget,
            &excluded,
            memory_context,
        )
        .await;
    }

    let response = match first_try {
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
                // The SARIF stays valid, but an incomplete review is not a pass (#720).
                let code = incomplete_exit_code(EXIT_OK, true, config.review.fail_on_incomplete);
                return Ok(ReviewResult::empty(code, format!("{warning_output}\n")));
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
    append_coverage(&mut output, format, &excluded, &[], opts.quiet);

    // 8. Quality gate output, debt snapshot, review history (best-effort)
    if let Some(gate) = &shaped.gate {
        output.push_str(&quality_gate::format_gate_output(gate));
    }
    finalize::save_history(
        config,
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
    // A judge that fell back to deterministic findings did not review the diff (#720).
    let exit_code = incomplete_exit_code(
        shaped.exit_code,
        response.judge_failure.is_some(),
        config.review.fail_on_incomplete,
    );

    // Leave a verifiable trace for the commit gate (#674): a staged diff that
    // was really reviewed (not a failed LLM call) may be committed by the
    // pre-commit hook without a second review.
    // Only a threshold at least as strict as the hook's (major) may stand in for the hook's review.
    if opts.staged && min_severity >= Severity::Major && response.judge_failure.is_none() {
        let judged_by = match judge {
            JudgeChoice::Llm(_) => "llm",
            JudgeChoice::Deterministic => "deterministic",
        };
        crate::engine::commit_gate::record_for_diff(
            &full_diff,
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
    excluded: &[String],
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
                "📦 Diff is {} chars (request budget {}), splitting into {} chunks…",
                diff.len(),
                max_size,
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
    let mut failures: Vec<ChunkFailure> = Vec::new();
    let mut any_success = false;
    let mut should_block = false;
    let mut dropped_findings = 0usize;

    let mut queue: std::collections::VecDeque<(chunker::DiffChunk, u8)> =
        chunks.iter().cloned().map(|c| (c, 0)).collect();
    while let Some((chunk, depth)) = queue.pop_front() {
        let chunk = &chunk;
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

                // The judge fell back to deterministic findings. Try the chunk
                // again in two halves before giving up on it (#718); the pieces
                // re-run the deterministic checks, so this attempt is dropped.
                if resp.judge_failure.is_some() && depth < MAX_RETRY_DEPTH {
                    if let Some(parts) = split_for_retry(chunk) {
                        retry_note(opts, chunk, parts.len());
                        for part in parts.into_iter().rev() {
                            queue.push_front((part, depth + 1));
                        }
                        continue;
                    }
                }
                // Only a chunk that is kept speaks in the merged summary; the
                // "LLM review failed" text of a chunk that was split and
                // retried must not survive into a run that then succeeded.
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
                // The judge fell back to deterministic findings: keep them, but
                // the chunk was not reviewed by the model (#720).
                if let Some(reason) = &resp.judge_failure {
                    any_error = true;
                    failures.push(ChunkFailure {
                        index: chunk.index,
                        total: chunk.total,
                        files: chunk.files.clone(),
                        error: reason.clone(),
                    });
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
                if depth < MAX_RETRY_DEPTH {
                    if let Some(parts) = split_for_retry(chunk) {
                        retry_note(opts, chunk, parts.len());
                        for part in parts.into_iter().rev() {
                            queue.push_front((part, depth + 1));
                        }
                        continue;
                    }
                }
                any_error = true;
                failures.push(ChunkFailure {
                    index: chunk.index,
                    total: chunk.total,
                    files: chunk.files.clone(),
                    error: e.to_string(),
                });
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
        judge_failure: None,
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
    append_coverage(&mut output, format, excluded, &failures, opts.quiet);

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
    finalize::save_history(
        config,
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
    // A review that skipped part of the diff is not a pass (#720).
    let exit_code = incomplete_exit_code(
        shaped.exit_code,
        !failures.is_empty(),
        config.review.fail_on_incomplete,
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

/// A chunk is split again after a model failure only down to this size.
const MIN_RETRY_SPLIT_CHARS: usize = 4_000;

/// How many times a failing chunk may be halved (so at most 1/4 of its size).
const MAX_RETRY_DEPTH: u8 = 2;

/// Split a chunk the model failed on into smaller ones, or `None` when it is
/// already small or is a single hunk that cannot be cut (#718).
fn split_for_retry(chunk: &chunker::DiffChunk) -> Option<Vec<chunker::DiffChunk>> {
    if chunk.diff.len() < MIN_RETRY_SPLIT_CHARS {
        return None;
    }
    let parts = chunker::chunk_diff(&chunk.diff, chunk.diff.len() / 2);
    (parts.len() > 1).then_some(parts)
}

/// Budget for re-reviewing a whole diff in smaller chunks after a failed
/// single request: half its size, or `None` when it cannot be split (#718).
fn retry_budget(diff: &str) -> Option<usize> {
    if diff.len() < MIN_RETRY_SPLIT_CHARS {
        return None;
    }
    let half = diff.len() / 2;
    (chunker::chunk_diff(diff, half).len() > 1).then_some(half)
}

fn retry_note(opts: &ReviewOptions, chunk: &chunker::DiffChunk, pieces: usize) {
    if !opts.quiet {
        eprintln!(
            "  {}",
            format!(
                "↻ Chunk {}/{} failed; retrying as {pieces} smaller pieces…",
                chunk.index + 1,
                chunk.total
            )
            .yellow()
        );
    }
}

/// A chunk the model could not review.
#[derive(Debug, Clone)]
struct ChunkFailure {
    index: usize,
    total: usize,
    files: Vec<String>,
    error: String,
}

/// Exit code once review coverage is known (#720): a run that did not cover
/// the whole diff exits 1 instead of 0 when `review.fail_on_incomplete` is on.
/// A blocking code (2) or an error code is never lowered.
fn incomplete_exit_code(code: i32, incomplete: bool, fail_on_incomplete: bool) -> i32 {
    if incomplete && fail_on_incomplete && code == EXIT_OK {
        EXIT_ERROR
    } else {
        code
    }
}

/// How many file names a coverage note lists before it says "+N more".
const COVERAGE_LIST_MAX: usize = 10;

fn name_list(files: &[String]) -> String {
    let shown: Vec<&str> = files
        .iter()
        .take(COVERAGE_LIST_MAX)
        .map(String::as_str)
        .collect();
    let mut s = shown.join(", ");
    if files.len() > COVERAGE_LIST_MAX {
        s.push_str(&format!(", +{} more", files.len() - COVERAGE_LIST_MAX));
    }
    s
}

/// Plain-text note naming the files a review did not cover: excluded by
/// `review.exclude`, or in chunks that failed. Empty when nothing was skipped.
fn coverage_note(excluded: &[String], failures: &[ChunkFailure]) -> String {
    let mut note = String::new();
    if !failures.is_empty() {
        let mut files: Vec<String> = failures.iter().flat_map(|f| f.files.clone()).collect();
        files.sort();
        files.dedup();
        let chunks: Vec<String> = failures
            .iter()
            .map(|f| format!("{}/{}", f.index + 1, f.total))
            .collect();
        note.push_str(&format!(
            "⚠️  Review incomplete: chunk {} failed ({}). Not reviewed: {}\n",
            chunks.join(", "),
            failures[0].error.lines().next().unwrap_or("error"),
            name_list(&files)
        ));
    }
    if !excluded.is_empty() {
        note.push_str(&format!(
            "ℹ️  Excluded by review.exclude ({}): {}\n",
            excluded.len(),
            name_list(excluded)
        ));
    }
    note
}

/// Add the coverage note to the output. Machine formats (JSON, SARIF,
/// compact) must stay parseable, so there it goes to stderr instead.
fn append_coverage(
    output: &mut String,
    format: OutputFormat,
    excluded: &[String],
    failures: &[ChunkFailure],
    quiet: bool,
) {
    let note = coverage_note(excluded, failures);
    if note.is_empty() {
        return;
    }
    if format == OutputFormat::Pretty {
        output.push('\n');
        output.push_str(&note);
    } else if !quiet {
        eprint!("{note}");
    }
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

    // ─── #720: incomplete reviews are not passes ───

    #[test]
    fn incomplete_review_fails_a_clean_run() {
        assert_eq!(incomplete_exit_code(EXIT_OK, true, true), EXIT_ERROR);
    }

    #[test]
    fn incomplete_exit_code_never_lowers_or_changes_other_codes() {
        assert_eq!(incomplete_exit_code(EXIT_BLOCKED, true, true), EXIT_BLOCKED);
        assert_eq!(incomplete_exit_code(EXIT_OK, false, true), EXIT_OK);
        assert_eq!(incomplete_exit_code(EXIT_OK, true, false), EXIT_OK);
    }

    #[test]
    fn coverage_note_names_failed_and_excluded_files() {
        let failures = vec![ChunkFailure {
            index: 1,
            total: 3,
            files: vec!["src/b.rs".into(), "src/a.rs".into()],
            error: "gateway timeout\nretry later".into(),
        }];
        let note = coverage_note(&["CHANGELOG.md".to_string()], &failures);
        assert!(
            note.contains("chunk 2/3 failed (gateway timeout)"),
            "{note}"
        );
        assert!(note.contains("Not reviewed: src/a.rs, src/b.rs"), "{note}");
        assert!(
            note.contains("Excluded by review.exclude (1): CHANGELOG.md"),
            "{note}"
        );
        assert!(coverage_note(&[], &[]).is_empty());
    }

    #[test]
    fn coverage_note_caps_long_lists() {
        let many: Vec<String> = (0..25).map(|i| format!("f{i}.md")).collect();
        let note = coverage_note(&many, &[]);
        assert!(note.contains("+15 more"), "{note}");
    }

    #[test]
    fn machine_formats_keep_stdout_clean() {
        let mut out = String::from("{}");
        append_coverage(&mut out, OutputFormat::Json, &["x.md".into()], &[], true);
        assert_eq!(out, "{}");
        let mut pretty = String::from("body");
        append_coverage(
            &mut pretty,
            OutputFormat::Pretty,
            &["x.md".into()],
            &[],
            true,
        );
        assert!(pretty.contains("Excluded by review.exclude (1): x.md"));
    }

    // ─── #718: smaller pieces after a model failure ───

    fn hunky(n: usize) -> String {
        let mut d =
            String::from("diff --git a/src/x.rs b/src/x.rs\n--- a/src/x.rs\n+++ b/src/x.rs\n");
        for i in 0..n {
            d.push_str(&format!(
                "@@ -{0},1 +{0},2 @@\n-old line number {i} padding padding padding\n+new line number {i} padding padding padding\n+extra\n",
                i * 10 + 1
            ));
        }
        d
    }

    #[test]
    fn a_failed_big_chunk_is_split_into_smaller_pieces() {
        let diff = hunky(200);
        assert!(diff.len() > MIN_RETRY_SPLIT_CHARS);
        let chunk = chunker::chunk_diff(&diff, usize::MAX).remove(0);
        let parts = split_for_retry(&chunk).expect("splittable");
        assert!(parts.len() >= 2);
        let hunks: usize = parts.iter().map(|p| p.diff.matches("\n@@ ").count()).sum();
        assert_eq!(hunks, 200, "no hunk lost or duplicated");
    }

    #[test]
    fn small_or_indivisible_chunks_are_not_retried() {
        let small = chunker::chunk_diff(&hunky(2), usize::MAX).remove(0);
        assert!(split_for_retry(&small).is_none());
        assert!(retry_budget(&hunky(2)).is_none());
        // One huge hunk cannot be cut below its own size.
        let one = format!(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,1 +1,1 @@\n+{}\n",
            "x".repeat(10_000)
        );
        assert!(retry_budget(&one).is_none());
    }

    #[test]
    fn retry_budget_is_half_the_diff() {
        let diff = hunky(200);
        assert_eq!(retry_budget(&diff), Some(diff.len() / 2));
    }
}
