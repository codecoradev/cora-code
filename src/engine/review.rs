use crate::error::CoraError;
use tracing::{debug, instrument};

use crate::config::schema::Config;
use crate::engine::comment_sanitizer;
use crate::engine::llm;
use crate::engine::types::{LLMConfig, ReviewIssue, ReviewResponse, Severity};

/// Load a custom system prompt from a file path.
/// Returns the file content, or None if the file doesn't exist, can't be read,
/// or is outside the project root (path traversal guard).
fn load_system_prompt_file(path: &str) -> Option<String> {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        tracing::debug!(path = path, "system_prompt_file does not exist");
        return None;
    };
    let project_root = std::env::current_dir().ok()?;
    let project_root = std::fs::canonicalize(&project_root).ok()?;

    if !canonical.starts_with(&project_root) {
        tracing::warn!(
            path = path,
            "system_prompt_file is outside project root, ignoring (potential path traversal)"
        );
        return None;
    }

    match std::fs::read_to_string(&canonical) {
        Ok(content) => Some(content),
        Err(e) => {
            tracing::warn!(
                path = path,
                error = %e,
                "failed to read system_prompt_file, using default prompt"
            );
            None
        }
    }
}

/// Resolve the effective system prompt: inline override > file override > None (use default).
pub fn resolve_system_prompt(inline: Option<&str>, file_path: Option<&str>) -> Option<String> {
    if let Some(prompt) = inline {
        Some(prompt.to_string())
    } else if let Some(path) = file_path {
        load_system_prompt_file(path)
    } else {
        None
    }
}

/// Run a code review on the given diff string with optional streaming and cache control.
///
/// When `stream` is true, LLM tokens are printed to stdout in real-time.
/// When `use_cache` is false, the cache is bypassed.
#[instrument(skip_all)]
pub async fn review_diff_with_cache(
    config: &Config,
    llm_config: &LLMConfig,
    diff: &str,
    stream: bool,
    use_cache: bool,
    quiet: bool,
    memory_context: Option<&str>,
) -> std::result::Result<ReviewResponse, CoraError> {
    review_diff_inner(
        config,
        llm_config,
        diff,
        stream,
        use_cache,
        quiet,
        memory_context,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn review_diff_inner(
    config: &Config,
    llm_config: &LLMConfig,
    diff: &str,
    stream: bool,
    use_cache: bool,
    quiet: bool,
    memory_context: Option<&str>,
) -> std::result::Result<ReviewResponse, CoraError> {
    debug!(
        diff_len = diff.len(),
        stream = stream,
        "starting diff review"
    );

    if diff.trim().is_empty() {
        return Ok(ReviewResponse {
            issues: vec![],
            summary: "No changes to review.".to_string(),
            tokens_used: None,
            should_block: false,
        });
    }

    // Check cache before calling LLM
    if use_cache {
        if let Some(cached) = crate::engine::cache::get_cached_review(
            diff,
            &llm_config.model,
            llm_config.temperature,
            config.cache_ttl,
            &llm_config.provider,
            &llm_config.base_url,
        ) {
            debug!("returning cached review response");
            return Ok(cached);
        }
    }

    // Extract valid file paths for post-parse filtering
    let valid_files = llm::extract_file_paths_from_diff(diff);

    // Resolve custom system prompt for review
    let review_prompt = resolve_system_prompt(
        config.review_system_prompt_override.as_deref(),
        config.review_system_prompt_file.as_deref(),
    );

    // Collect static analysis context (clippy output, etc.)
    let static_context =
        crate::engine::static_analysis::collect_static_context(diff, &config.static_analysis);

    // Parse diff and run rule engine. Deterministic scanners (rules, secrets,
    // security) always operate on the ORIGINAL unsanitized diff — only the
    // LLM sees sanitized text (ALIBI defense, arXiv:2607.24964).
    let diff_chunks = crate::engine::diff_parser::parse_diff(diff);
    let sanitize_report = crate::engine::comment_sanitizer::flag_claims(&diff_chunks);
    let review_diff_text: std::borrow::Cow<'_, str> = if config.sanitize_comments {
        let mut sanitized_chunks = crate::engine::diff_parser::parse_diff(diff);
        let full_report = crate::engine::comment_sanitizer::sanitize_chunks(&mut sanitized_chunks);
        let rendered = crate::engine::comment_sanitizer::render_sanitized_diff(&sanitized_chunks);
        debug!(
            sanitized = full_report.lines_sanitized,
            claims = full_report.suspicious_claims.len(),
            "ALIBI comment defense applied"
        );
        if rendered.is_empty() {
            std::borrow::Cow::Borrowed(diff)
        } else {
            std::borrow::Cow::Owned(rendered)
        }
    } else {
        if !sanitize_report.suspicious_claims.is_empty() {
            debug!(
                claims = sanitize_report.suspicious_claims.len(),
                "Untrusted verification claims flagged in added comments"
            );
        }
        std::borrow::Cow::Borrowed(diff)
    };

    let rule_findings = crate::engine::rules::run_rules(&diff_chunks, &config.rules_config);

    // Run deterministic secrets pre-scan
    let secrets_findings = crate::engine::secrets_scanner::scan_secrets(
        &diff_chunks,
        config.rules_config.max_findings,
    );

    // Run deterministic security pattern scan (weak crypto, injection, etc.)
    let security_findings = crate::engine::security_scanner::scan_security(
        &diff_chunks,
        config.rules_config.max_findings,
    );

    // Run index-powered scans (requires symbol graph — graceful no-op without index)
    let skip_patterns = &config.rules_config.index_skip_files;
    let index_unused_findings = crate::engine::index_scanner::scan_unused_imports(
        &diff_chunks,
        std::env::current_dir().unwrap_or_default().as_path(),
        config.rules_config.max_findings,
        skip_patterns,
    );
    let index_dead_findings = crate::engine::index_scanner::scan_dead_code_in_review(
        &diff_chunks,
        std::env::current_dir().unwrap_or_default().as_path(),
        config.rules_config.max_findings,
        skip_patterns,
    );
    let index_breaking_findings = crate::engine::index_scanner::scan_breaking_changes(
        &diff_chunks,
        std::env::current_dir().unwrap_or_default().as_path(),
        config.rules_config.max_findings,
        skip_patterns,
    );

    let rule_context = crate::engine::rules::format_rule_context(&rule_findings);
    let secrets_context = crate::engine::rules::format_rule_context(&secrets_findings);
    let security_context = crate::engine::rules::format_rule_context(&security_findings);
    let index_unused_context = crate::engine::rules::format_rule_context(&index_unused_findings);
    let index_dead_context = crate::engine::rules::format_rule_context(&index_dead_findings);
    let index_breaking_context =
        crate::engine::rules::format_rule_context(&index_breaking_findings);
    // Keep a clone for merging after LLM (rule_findings may be consumed in error fallback)
    let rule_findings_clone = rule_findings.clone();
    let secrets_findings_clone = secrets_findings.clone();
    let security_findings_clone = security_findings.clone();
    let index_unused_findings_clone = index_unused_findings.clone();
    let index_dead_findings_clone = index_dead_findings.clone();
    let index_breaking_findings_clone = index_breaking_findings.clone();

    // Combine all context sections for LLM prompt (static analysis + all scanner findings)
    let mut context_parts: Vec<String> = Vec::new();
    if let Some(sa) = static_context.as_deref() {
        context_parts.push(sa.to_string());
    }
    if let Some(warning) = comment_sanitizer::format_claim_warning(&sanitize_report) {
        context_parts.push(warning);
    }
    for ctx in [
        rule_context.as_str(),
        secrets_context.as_str(),
        security_context.as_str(),
        index_unused_context.as_str(),
        index_dead_context.as_str(),
        index_breaking_context.as_str(),
    ] {
        if !ctx.is_empty() {
            context_parts.push(ctx.to_string());
        }
    }
    let combined_context = if context_parts.is_empty() {
        None
    } else {
        Some(context_parts.join("\n\n"))
    };

    // Build context chain (cross-file dependency extraction)
    // NOTE: pass ignore.files (e.g. target/**, node_modules/**) so the resolver
    // never injects build-artifact code — not ignore.rules (finding-type strings).
    let context_chain = crate::engine::context::build_context_chain(
        &diff_chunks,
        &config.context_chain,
        std::env::current_dir().unwrap_or_default().as_path(),
        &config.ignore.files,
    );

    let final_context = if !context_chain.text.is_empty() {
        match combined_context {
            Some(ctx) => Some(format!(
                "{ctx}\n\n## Cross-file Context\n{context_chain_text}",
                context_chain_text = context_chain.text
            )),
            None => Some(format!("## Cross-file Context\n{}", context_chain.text)),
        }
    } else {
        combined_context
    };

    // Inject language-specific context (reuses parsed diff_chunks)
    let lang_context =
        crate::engine::language_analyzer::build_language_context_from_chunks(&diff_chunks);
    let final_context = if !lang_context.is_empty() {
        match final_context {
            Some(ctx) => Some(format!("{lang_context}\n\n{ctx}")),
            None => Some(lang_context),
        }
    } else {
        final_context
    };

    // Inject profile instructions into the context
    let final_context = match (&config.profile, final_context) {
        (Some(profile), Some(ctx)) => {
            let profile_prompt = crate::engine::profiles::build_profile_prompt(profile);
            Some(format!("## Quality Profile\n{profile_prompt}\n\n{ctx}"))
        }
        (Some(profile), None) => {
            let profile_prompt = crate::engine::profiles::build_profile_prompt(profile);
            Some(format!("## Quality Profile\n{profile_prompt}"))
        }
        (None, ctx) => ctx,
    };

    // Inject memory context from Uteke (if --memory flag was used)
    let final_context = match (memory_context, final_context) {
        (Some(mem), Some(ctx)) => Some(format!("{mem}\n\n{ctx}")),
        (Some(mem), None) => Some(mem.to_string()),
        (None, ctx) => ctx,
    };

    // ── Brain enrichment phase (Tier 1) ──────────────────────────────────
    // When use_brain is enabled and an index exists, enrich the review prompt
    // with impact analysis, affected tests, and semantic pattern search.
    let final_context = if config.context_chain.use_brain {
        match build_brain_context(
            &diff_chunks,
            config.context_chain.impact_depth,
            std::env::current_dir().unwrap_or_default().as_path(),
        ) {
            Some(brain_ctx) if !brain_ctx.is_empty() => {
                debug!(
                    brain_context_len = brain_ctx.len(),
                    "brain enrichment applied"
                );
                match final_context {
                    Some(ctx) => Some(format!(
                        "{ctx}\n\n## Code Intelligence (Brain)\n{brain_ctx}"
                    )),
                    None => Some(format!("## Code Intelligence (Brain)\n{brain_ctx}")),
                }
            }
            _ => final_context,
        }
    } else {
        final_context
    }; // but preserve deterministic rule findings even on LLM failure
    let llm_result: Result<ReviewResponse, CoraError> = if stream {
        llm::review_diff_stream(
            llm_config,
            &review_diff_text,
            &config.focus,
            &config.rules,
            &config.response_format,
            review_prompt.as_deref(),
            final_context.as_deref(),
        )
        .await
    } else {
        llm::review_diff(
            llm_config,
            &review_diff_text,
            &config.focus,
            &config.rules,
            &config.response_format,
            review_prompt.as_deref(),
            quiet,
            final_context.as_deref(),
        )
        .await
    };

    let mut response = match llm_result {
        Ok(resp) => resp,
        Err(e) => {
            // LLM failed — return deterministic findings only (don't silently swallow them)
            if !rule_findings.is_empty()
                || !secrets_findings.is_empty()
                || !security_findings.is_empty()
                || !index_unused_findings.is_empty()
                || !index_dead_findings.is_empty()
                || !index_breaking_findings.is_empty()
            {
                let n_rules = rule_findings.len();
                let n_secrets = secrets_findings.len();
                let n_security = security_findings.len();
                let n_index_unused = index_unused_findings.len();
                let n_index_dead = index_dead_findings.len();
                let n_index_breaking = index_breaking_findings.len();
                debug!(
                    error = %e,
                    rule_findings = n_rules,
                    secrets_findings = n_secrets,
                    security_findings = n_security,
                    index_unused = n_index_unused,
                    index_dead = n_index_dead,
                    index_breaking = n_index_breaking,
                    "LLM call failed, returning deterministic findings only"
                );
                let mut all_deterministic =
                    crate::engine::rules::merge_rule_findings(vec![], rule_findings);
                all_deterministic =
                    crate::engine::rules::merge_rule_findings(all_deterministic, secrets_findings);
                all_deterministic =
                    crate::engine::rules::merge_rule_findings(all_deterministic, security_findings);
                all_deterministic = crate::engine::rules::merge_rule_findings(
                    all_deterministic,
                    index_unused_findings,
                );
                all_deterministic = crate::engine::rules::merge_rule_findings(
                    all_deterministic,
                    index_dead_findings,
                );
                all_deterministic = crate::engine::rules::merge_rule_findings(
                    all_deterministic,
                    index_breaking_findings,
                );
                let mut fallback = ReviewResponse {
                    issues: all_deterministic,
                    summary: format!(
                        "LLM review failed: {e}. Showing {n_rules} rule + {n_secrets} secrets + {n_security} security + {n_index_unused} unused imports + {n_index_dead} dead code + {n_index_breaking} breaking changes."
                    ),
                    tokens_used: None,
                    should_block: false,
                };
                fallback.issues = apply_markdown_code_block_filter(fallback.issues, &diff_chunks);
                fallback.issues = apply_ignore_rules(fallback.issues, &config.ignore.rules);
                let min_sev = config.hook.min_severity_level();
                fallback.should_block = fallback
                    .issues
                    .iter()
                    .any(|issue| issue.severity <= min_sev);
                return Ok(fallback);
            }
            return Err(e);
        }
    };

    // Merge rule findings + secrets findings + security findings + index findings with LLM issues
    if !rule_findings_clone.is_empty() {
        response.issues =
            crate::engine::rules::merge_rule_findings(response.issues, rule_findings_clone);
    }
    if !secrets_findings_clone.is_empty() {
        response.issues =
            crate::engine::rules::merge_rule_findings(response.issues, secrets_findings_clone);
    }
    if !security_findings_clone.is_empty() {
        response.issues =
            crate::engine::rules::merge_rule_findings(response.issues, security_findings_clone);
    }
    if !index_unused_findings_clone.is_empty() {
        response.issues =
            crate::engine::rules::merge_rule_findings(response.issues, index_unused_findings_clone);
    }
    if !index_dead_findings_clone.is_empty() {
        response.issues =
            crate::engine::rules::merge_rule_findings(response.issues, index_dead_findings_clone);
    }
    if !index_breaking_findings_clone.is_empty() {
        response.issues = crate::engine::rules::merge_rule_findings(
            response.issues,
            index_breaking_findings_clone,
        );
    }

    // Filter out issues with invalid file paths (hallucination guard)
    if !valid_files.is_empty() {
        let before = response.issues.len();
        response
            .issues
            .retain(|issue| is_valid_file_path(&issue.file, &valid_files));
        let filtered = before - response.issues.len();
        if filtered > 0 {
            debug!(
                filtered,
                remaining = response.issues.len(),
                "filtered issues with invalid file paths"
            );
        }
    }

    // Cross-validate LLM security findings about hardcoded secrets against
    // actual diff lines. The LLM sometimes flags struct field declarations
    // (e.g. `api_key: String`) as "hardcoded secret" even when no literal
    // value is present. This filter removes such false positives by checking
    // the added line at the reported file:line against the built-in
    // sec-hardcoded-secret regex.
    response.issues = apply_llm_secret_fp_filter(response.issues, &diff_chunks);

    // Drop findings inside Markdown fenced code blocks (#329). Code blocks in
    // `.md` files are documentation examples, not executable code — a `git push`
    // inside a ```bash block is not SQL injection.
    response.issues = apply_markdown_code_block_filter(response.issues, &diff_chunks);

    // Apply ignore rules: filter out issues matching ignored patterns
    response.issues = apply_ignore_rules(response.issues, &config.ignore.rules);

    // Drop low-severity findings on unchanged (context) lines — these are
    // pre-existing code that appeared in the diff due to surrounding changes,
    // not new code introduced by the PR (#507 Pattern #3).
    response.issues = apply_context_line_filter(response.issues, &diff_chunks);

    // Calculate should_block based on min_severity
    let min_severity = config.hook.min_severity_level();
    // Ord order is Critical(0) < Major(1) < Minor(2) < Info(3), so "at or above
    // min_severity" means Ord value <= min_severity.
    response.should_block = response
        .issues
        .iter()
        .any(|issue| issue.severity <= min_severity);

    debug!(
        issues = response.issues.len(),
        should_block = response.should_block,
        "review complete"
    );

    // Save fully-processed response to cache (after filtering)
    if use_cache {
        if let Err(e) = crate::engine::cache::save_cached_review(
            diff,
            &llm_config.model,
            llm_config.temperature,
            &response,
            &llm_config.provider,
            &llm_config.base_url,
        ) {
            debug!("failed to save review to cache: {}", e);
        }
    }

    Ok(response)
}

/// Filter out LLM findings about hardcoded secrets/passwords that point to
/// diff lines which don't actually contain a literal string assignment.
///
/// The LLM sometimes flags struct field declarations like `api_key: String`
/// or `api_key: extract_api_key.clone()` as "Hardcoded password or secret in
/// variable". These are identifiers, not hardcoded values.
///
/// This function cross-validates each security finding against the actual
/// added line in the diff. If the line doesn't match the `sec-hardcoded-secret`
/// regex (i.e. no `password/key/secret = "literal"` pattern), the finding is
/// removed as a false positive.
fn apply_llm_secret_fp_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[crate::engine::diff_parser::FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::diff_parser::DiffLineType;

    // Lazy-compiled regex matching the built-in sec-hardcoded-secret pattern.
    // Only triggers for actual value assignments like `api_key = "sk-..."`.
    static RE_SECRET_LITERAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(?:password|api_?key|token|secret)\s*=\s*"[^"]+""#)
            .expect("hardcoded secret regex must compile")
    });

    // Keywords that indicate an LLM finding is about hardcoded secrets.
    static SECRET_KEYWORDS: &[&str] = &[
        "hardcoded password",
        "hardcoded secret",
        "hardcoded credential",
        "hardcoded token",
        "hardcoded api key",
        "hardcoded api_key",
    ];

    // Pre-compute a lookup: (file_path, new_line_no) -> line content
    let added_lines: std::collections::HashMap<(String, u32), &str> = diff_chunks
        .iter()
        .flat_map(|chunk| {
            let path = chunk
                .new_path
                .as_deref()
                .or(chunk.old_path.as_deref())
                .unwrap_or("unknown");
            chunk.chunks.iter().flat_map(|hunk| {
                hunk.lines
                    .iter()
                    .filter(|l| l.line_type == DiffLineType::Add)
                    .filter_map(|l| {
                        l.new_line_no
                            .map(|ln| ((path.to_string(), ln), l.content.as_str()))
                    })
            })
        })
        .collect();

    let before = issues.len();
    issues.retain(|issue| {
        // Only check security-type findings about secrets
        let issue_type = issue.issue_type.as_deref().unwrap_or("");
        let title_lower = issue.title.to_lowercase();

        if issue_type != "security" {
            return true;
        }

        let is_secret_finding = SECRET_KEYWORDS.iter().any(|kw| title_lower.contains(kw));
        if !is_secret_finding {
            return true;
        }

        // Look up the actual diff line
        let line_num = issue.line.unwrap_or(0);
        let key = (issue.file.clone(), line_num);
        if let Some(actual_line) = added_lines.get(&key) {
            if !RE_SECRET_LITERAL.is_match(actual_line) {
                debug!(
                    file = %issue.file,
                    line = line_num,
                    title = %issue.title,
                    "suppressed LLM false positive: line has no hardcoded secret literal"
                );
                return false; // Remove this finding
            }
        }

        // If we can't find the line (hallucinated path/line or context line),
        // keep the finding — better safe than sorry.
        true
    });

    let filtered = before - issues.len();
    if filtered > 0 {
        debug!(
            filtered,
            remaining = issues.len(),
            "filtered LLM false positives for hardcoded secret findings"
        );
    }

    issues
}

/// Drop findings located inside Markdown fenced code blocks (#329).
///
/// Code blocks in `.md`/`.mdx`/`.markdown` files are documentation examples,
/// not executable code — e.g. a `git push` inside a ```bash block must not be
/// flagged as SQL injection. Findings without a resolvable line, or in files
/// without any code block, are kept unchanged (safe default).
fn apply_markdown_code_block_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[crate::engine::diff_parser::FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::markdown::{is_markdown, lines_inside_code_blocks};
    use std::collections::HashSet;

    // Build file path -> set of code-block line numbers, for markdown files only.
    let mut code_block_lines: std::collections::HashMap<String, HashSet<u32>> =
        std::collections::HashMap::new();
    for chunk in diff_chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        if !is_markdown(path) {
            continue;
        }
        let set = lines_inside_code_blocks(chunk);
        if !set.is_empty() {
            code_block_lines
                .entry(path.to_string())
                .or_default()
                .extend(set);
        }
    }

    if code_block_lines.is_empty() {
        return issues; // no markdown code blocks in this diff — fast path
    }

    let before = issues.len();
    issues.retain(|issue| {
        let Some(ln) = issue.line else {
            return true; // keep findings without a concrete line number
        };
        match code_block_lines.get(&issue.file) {
            Some(lines) => !lines.contains(&ln), // drop if inside a code block
            None => true,
        }
    });

    let dropped = before - issues.len();
    if dropped > 0 {
        debug!(
            dropped,
            remaining = issues.len(),
            "removed markdown code-block false positives"
        );
    }

    issues
}

/// Filter out issues whose `issue_type` matches any ignored rule pattern.
fn apply_ignore_rules(mut issues: Vec<ReviewIssue>, ignore_rules: &[String]) -> Vec<ReviewIssue> {
    if ignore_rules.is_empty() {
        return issues;
    }

    let before = issues.len();
    issues.retain(|issue| {
        !ignore_rules.iter().any(|pattern| {
            let pattern_lower = pattern.to_lowercase();
            let issue_type_lower = issue.issue_type.clone().unwrap_or_default().to_lowercase();
            issue_type_lower.contains(&pattern_lower)
                || issue.title.to_lowercase().contains(&pattern_lower)
        })
    });
    let filtered = before - issues.len();
    if filtered > 0 {
        debug!(
            filtered,
            remaining = issues.len(),
            rules = ignore_rules.len(),
            "filtered issues via ignore rules"
        );
    }

    issues
}

/// Drop findings on unchanged (context) or removed lines (#507 Pattern #3).
///
/// The LLM sometimes flags pre-existing code that appears in the diff purely
/// because surrounding lines changed. These findings are not about code the PR
/// introduces — they are noise.
///
/// **Policy:** Only drop `Minor` and `Info` severity findings on context/removed
/// lines. `Critical` and `Major` findings are kept regardless, because they may
/// represent real risks worth surfacing even in pre-existing code.
fn apply_context_line_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[crate::engine::diff_parser::FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::diff_parser::DiffLineType;

    // Build lookup: (file, new_line_no) -> is_added
    // Only includes lines present in the diff (Add or Context). Lines not in
    // the diff at all are left alone (LLM line numbers can be imprecise).
    let mut line_kinds: std::collections::HashMap<(String, u32), DiffLineType> =
        std::collections::HashMap::new();
    for chunk in diff_chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        for hunk in &chunk.chunks {
            for line in &hunk.lines {
                if let Some(ln) = line.new_line_no {
                    line_kinds.insert((path.to_string(), ln), line.line_type);
                }
            }
        }
    }

    let before = issues.len();
    issues.retain(|issue| {
        // Keep findings without a concrete line number
        let Some(ln) = issue.line else {
            return true;
        };

        // Only filter if we can resolve this (file, line) to a diff line
        let Some(kind) = line_kinds.get(&(issue.file.clone(), ln)) else {
            return true; // not in diff — can't determine, keep
        };

        match kind {
            DiffLineType::Add => true, // genuinely new code — always keep
            DiffLineType::Context | DiffLineType::Remove => {
                // Pre-existing code — only keep if severity is high enough
                // Ord: Critical(0) < Major(1) < Minor(2) < Info(3)
                issue.severity <= Severity::Major
            }
        }
    });

    let dropped = before - issues.len();
    if dropped > 0 {
        debug!(
            dropped,
            remaining = issues.len(),
            "removed low-severity findings on unchanged diff context lines (#507)"
        );
    }

    issues
}

/// Check if a file path from an LLM issue matches any of the valid diff file paths.
/// Uses exact match only — the LLM should report paths exactly as they appear in the diff.
fn is_valid_file_path(issue_file: &str, valid_files: &[String]) -> bool {
    valid_files.iter().any(|f| f == issue_file)
}

/// Build brain-enriched context from the symbol index.
///
/// Queries the index for:
/// 1. **Impact analysis** — blast radius of changed symbols (who depends on them)
/// 2. **Affected tests** — test files that exercise the changed code
/// 3. **Brain search** — semantically related patterns across the codebase
///
/// Returns `None` if no index is available or no results found.
pub(crate) fn build_brain_context(
    diff_chunks: &[crate::engine::diff_parser::FileChunk],
    impact_depth: u32,
    project_root: &std::path::Path,
) -> Option<String> {
    // Try to open the global symbol index
    let conn = crate::index::open_global_index().ok()?;
    let project_id = crate::index::ensure_project(&conn, project_root).ok()?;

    // Extract defined symbols from the diff
    let defs = crate::engine::context::extraction::extract_definitions_from_diff(diff_chunks);
    if defs.is_empty() {
        return None;
    }

    let mut sections = Vec::new();

    // ── 1. Impact Analysis ─────────────────────────────────────────────
    let mut impact_lines: Vec<String> = Vec::new();
    for def in &defs {
        if def.name.len() < 2 {
            continue;
        }
        if let Ok(nodes) =
            crate::index::graph::impact_analysis(&conn, project_id, &def.name, impact_depth)
        {
            if !nodes.is_empty() {
                impact_lines.push(format!(
                    "- `{}`: {} downstream caller(s)",
                    def.name,
                    nodes.len()
                ));
                // Show top callers (deduplicated by file)
                let mut seen_files = std::collections::HashSet::new();
                for node in nodes.iter().take(5) {
                    if seen_files.insert(node.file.clone()) {
                        impact_lines.push(format!(
                            "  - depth {}: {} ({}:{})",
                            node.depth, node.symbol, node.file, node.line
                        ));
                    }
                }
                if nodes.len() > 5 {
                    impact_lines.push(format!("  - ... and {} more", nodes.len() - 5));
                }
            }
        }
    }
    if !impact_lines.is_empty() {
        sections.push(format!(
            "### Impact Analysis (Blast Radius)\n{}",
            impact_lines.join("\n")
        ));
    }

    // ── 2. Affected Tests ───────────────────────────────────────────────
    let mut test_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for def in &defs {
        if def.name.len() < 2 {
            continue;
        }
        // Walk impact nodes, collect files containing "test" or "spec"
        if let Ok(nodes) = crate::index::graph::impact_analysis(
            &conn, project_id, &def.name, 1, // depth 1 is enough for test detection
        ) {
            for node in &nodes {
                let lower = node.file.to_lowercase();
                if lower.contains("test") || lower.contains("spec") || lower.contains("_test") {
                    test_files.insert(node.file.clone());
                }
            }
        }
        // Also search FTS5 for test symbols matching this function name
        if let Ok(results) =
            crate::index::brain::brain_search(&conn, project_id, &format!("test {}", def.name), 3)
        {
            for r in results {
                let lower = r.file.to_lowercase();
                if lower.contains("test") || lower.contains("spec") || lower.contains("_test") {
                    test_files.insert(r.file);
                }
            }
        }
    }
    if !test_files.is_empty() {
        let mut test_list: Vec<_> = test_files.into_iter().collect();
        test_list.sort();
        sections.push(format!(
            "### Potentially Affected Tests\n{}",
            test_list
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    // ── 3. Semantic Pattern Search ───────────────────────────────────────
    let mut brain_lines: Vec<String> = Vec::new();
    let mut seen_brain: std::collections::HashSet<String> = std::collections::HashSet::new();
    for def in defs.iter().take(5) {
        // limit to 5 symbols to avoid excessive token cost
        if def.name.len() < 2 {
            continue;
        }
        if let Ok(results) = crate::index::brain::brain_search(&conn, project_id, &def.name, 3) {
            for r in results {
                // Skip results from the same file as the definition
                if r.file == def.file {
                    continue;
                }
                if seen_brain.insert(format!("{}:{}", r.file, r.line)) {
                    brain_lines.push(format!(
                        "- `{}` in {}:{} (signals: {})",
                        r.name,
                        r.file,
                        r.line,
                        r.signals.join("+")
                    ));
                }
            }
        }
    }
    if !brain_lines.is_empty() {
        sections.push(format!(
            "### Related Patterns (Semantic Search)\n{}",
            brain_lines.join("\n")
        ));
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// Build brain context for `cora scan` from a list of files.
///
/// Unlike `build_brain_context` (which works on diff chunks), this variant
/// extracts symbols from the scanned file list and queries the index for
/// impact analysis, affected tests, and related patterns.
///
/// Returns `None` if no index is available or no results found.
pub(crate) fn build_scan_brain_context(
    files: &[crate::engine::scanner::FileEntry],
    impact_depth: u32,
    project_root: &std::path::Path,
) -> Option<String> {
    let conn = crate::index::open_global_index().ok()?;
    let project_id = crate::index::ensure_project(&conn, project_root).ok()?;

    // Extract function/type names from each file using simple heuristics.
    // For scan we don't have tree-sitter AST — we use the index's FTS5
    // to find symbols defined in these files.
    let mut sections = Vec::new();
    let file_paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // ── Single-pass: collect all symbols from scanned files ────────
    // Query brain_search once per file (capped at 10), then reuse the
    // collected symbols for both impact analysis and affected-tests lookup.
    let mut all_symbols: Vec<crate::index::brain::BrainResult> = Vec::new();
    for file_path in file_paths.iter().take(10) {
        let query = format!("file:\"{file_path}\"");
        if let Ok(results) = crate::index::brain::brain_search(&conn, project_id, &query, 5) {
            all_symbols.extend(results.into_iter().filter(|r| r.name.len() >= 2));
        }
    }

    // Deduplicate by name to avoid redundant impact_analysis calls
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let unique_symbols: Vec<_> = all_symbols
        .into_iter()
        .filter(|r| seen_names.insert(r.name.clone()))
        .collect();

    // ── 1. Impact Analysis ───────────────────────────────────────────
    let mut impact_lines: Vec<String> = Vec::new();
    for r in &unique_symbols {
        if let Ok(nodes) =
            crate::index::graph::impact_analysis(&conn, project_id, &r.name, impact_depth)
        {
            if nodes.len() > 2 {
                impact_lines.push(format!(
                    "- `{}` ({}:{}): {} downstream caller(s)",
                    r.name,
                    r.file,
                    r.line,
                    nodes.len()
                ));
            }
        }
    }
    if !impact_lines.is_empty() {
        sections.push(format!(
            "### High-Impact Symbols\n{}\n  Consider extra scrutiny for these high-call-count symbols.",
            impact_lines.join("\n")
        ));
    }

    // ── 2. Affected Tests ────────────────────────────────────────────
    // Reuse the same symbols — no additional brain_search calls needed.
    let mut test_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &unique_symbols {
        if let Ok(nodes) = crate::index::graph::impact_analysis(&conn, project_id, &r.name, 1) {
            for node in &nodes {
                let lower = node.file.to_lowercase();
                if lower.contains("test") || lower.contains("spec") || lower.contains("_test") {
                    test_files.insert(node.file.clone());
                }
            }
        }
    }
    if !test_files.is_empty() {
        let mut test_list: Vec<_> = test_files.into_iter().collect();
        test_list.sort();
        sections.push(format!(
            "### Potentially Affected Tests\n{}",
            test_list
                .iter()
                .map(|f| format!("- `{f}`"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;

    #[test]
    fn resolve_prompt_inline_takes_priority() {
        let result = resolve_system_prompt(Some("inline prompt"), Some("file.md"));
        assert_eq!(result.as_deref(), Some("inline prompt"));
    }

    #[test]
    fn resolve_prompt_file_fallback() {
        // Use a file within the project root so the path traversal guard allows it
        let test_file = std::path::PathBuf::from(".cora-test-prompt.tmp");
        std::fs::write(&test_file, "file prompt content").unwrap();
        let result = resolve_system_prompt(None, Some(".cora-test-prompt.tmp"));
        assert_eq!(result.as_deref(), Some("file prompt content"));
        let _ = std::fs::remove_file(&test_file);
    }

    #[test]
    fn resolve_prompt_none_when_both_missing() {
        let result = resolve_system_prompt(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_prompt_none_when_file_missing() {
        let result = resolve_system_prompt(None, Some("/nonexistent/prompt.md"));
        assert!(result.is_none());
    }

    #[test]
    fn reject_path_traversal_outside_project() {
        // /etc/passwd exists but is outside project root — should be rejected
        let result = resolve_system_prompt(None, Some("/etc/passwd"));
        assert!(
            result.is_none(),
            "system_prompt_file outside project root should be rejected"
        );
    }

    #[test]
    fn secret_fp_filter_removes_struct_field_declarations() {
        use crate::engine::diff_parser::*;

        // Simulate a diff with a struct field declaration (not a hardcoded secret)
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("crates/uteke-cli/src/cli.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 230,
                old_count: 0,
                new_start: 234,
                new_count: 2,
                header: "".to_string(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "    extract_api_key: Option<String>,".to_string(),
                        old_line_no: None,
                        new_line_no: Some(236),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "    extract_base_url: Option<String>,".to_string(),
                        old_line_no: None,
                        new_line_no: Some(237),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![ReviewIssue {
            file: "crates/uteke-cli/src/cli.rs".to_string(),
            line: Some(236),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "Hardcoded password or secret in variable".to_string(),
            body: "Static security scanner detected...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert!(
            result.is_empty(),
            "struct field declaration should be filtered out"
        );
    }

    #[test]
    fn secret_fp_filter_keeps_actual_hardcoded_secrets() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/config.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 10,
                old_count: 0,
                new_start: 15,
                new_count: 1,
                header: "".to_string(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "    let api_key = \"sk-12345abcdef\";".to_string(),
                    old_line_no: None,
                    new_line_no: Some(15),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![ReviewIssue {
            file: "src/config.rs".to_string(),
            line: Some(15),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "Hardcoded password or secret in variable".to_string(),
            body: "API key hardcoded...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "actual hardcoded secret should be kept");
    }

    #[test]
    fn secret_fp_filter_keeps_non_security_findings() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/main.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                header: "".to_string(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "    api_key: String,".to_string(),
                    old_line_no: None,
                    new_line_no: Some(1),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![ReviewIssue {
            file: "src/main.rs".to_string(),
            line: Some(1),
            severity: Severity::Minor,
            issue_type: Some("bugs".to_string()),
            title: "Use of unwrap()".to_string(),
            body: "This can panic".to_string(),
            suggested_fix: None,
        }];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "non-security findings should pass through");
    }

    #[test]
    fn secret_fp_filter_keeps_findings_with_unknown_lines() {
        use crate::engine::diff_parser::*;

        // Empty diff — finding references a line not in the diff
        let diff_chunks: Vec<FileChunk> = vec![];

        let issues = vec![ReviewIssue {
            file: "src/config.rs".to_string(),
            line: Some(999),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "Hardcoded password or secret in variable".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(
            result.len(),
            1,
            "unknown lines should be kept (better safe than sorry)"
        );
    }

    #[test]
    fn ignore_rules_filters_by_title_match() {
        let issues = vec![
            ReviewIssue {
                file: "cli.rs".to_string(),
                line: Some(236),
                severity: Severity::Critical,
                issue_type: Some("rule".to_string()),
                title: "Command injection via exec/system with dynamic input".to_string(),
                body: "Static security scanner detected...".to_string(),
                suggested_fix: None,
            },
            ReviewIssue {
                file: "main.rs".to_string(),
                line: Some(10),
                severity: Severity::Major,
                issue_type: Some("security".to_string()),
                title: "SQL injection via string concatenation".to_string(),
                body: "...".to_string(),
                suggested_fix: None,
            },
        ];

        let rules = vec!["Command injection via exec/system with dynamic input".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "SQL injection via string concatenation");
    }

    #[test]
    fn ignore_rules_filters_by_issue_type_match() {
        let issues = vec![ReviewIssue {
            file: "test.py".to_string(),
            line: Some(50),
            severity: Severity::Minor,
            issue_type: Some("style".to_string()),
            title: "Some style issue".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let rules = vec!["style".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert!(result.is_empty());
    }

    #[test]
    fn ignore_rules_empty_keeps_all() {
        let issues = vec![ReviewIssue {
            file: "f.rs".to_string(),
            line: Some(1),
            severity: Severity::Critical,
            issue_type: Some("rule".to_string()),
            title: "Any finding".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_ignore_rules(issues, &[]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn ignore_rules_case_insensitive() {
        let issues = vec![ReviewIssue {
            file: "f.rs".to_string(),
            line: Some(1),
            severity: Severity::Critical,
            issue_type: Some("rule".to_string()),
            title: "HARDCODED password or SECRET in variable".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let rules = vec!["Hardcoded Password Or Secret".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert!(result.is_empty());
    }

    // ─── #329: markdown fenced code-block false positives ───

    #[test]
    fn markdown_fp_filter_drops_finding_inside_code_block() {
        use crate::engine::diff_parser::*;

        // The exact #329 scenario: a `git push` inside a ```bash block in a
        // markdown doc, flagged as SQL injection.
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("AGENT.md".to_string()),
            language: "markdown".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 4,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```bash".to_string(),
                        old_line_no: None,
                        new_line_no: Some(167),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "git push origin vX.Y.Z".to_string(),
                        old_line_no: None,
                        new_line_no: Some(168),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```".to_string(),
                        old_line_no: None,
                        new_line_no: Some(169),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![ReviewIssue {
            file: "AGENT.md".to_string(),
            line: Some(168),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "SQL injection via string concatenation".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert!(
            result.is_empty(),
            "finding inside a markdown code block must be dropped"
        );
    }

    #[test]
    fn markdown_fp_filter_keeps_finding_outside_code_block() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("doc.md".to_string()),
            language: "markdown".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 3,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```bash".to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "echo hi".to_string(),
                        old_line_no: None,
                        new_line_no: Some(2),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```".to_string(),
                        old_line_no: None,
                        new_line_no: Some(3),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        // Finding on line 5 (outside the block, in prose) must survive.
        let issues = vec![ReviewIssue {
            file: "doc.md".to_string(),
            line: Some(5),
            severity: Severity::Minor,
            issue_type: Some("style".to_string()),
            title: "typo".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "finding outside a code block must be kept");
    }

    #[test]
    fn markdown_fp_filter_keeps_findings_in_non_markdown_files() {
        use crate::engine::diff_parser::*;

        // A real .py file is never treated as markdown, even if it has ``` text.
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/app.py".to_string()),
            language: "python".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 2,
                header: String::new(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "eval(request.body.code)".to_string(),
                    old_line_no: None,
                    new_line_no: Some(42),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![ReviewIssue {
            file: "src/app.py".to_string(),
            line: Some(42),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "eval injection".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "non-markdown files are unaffected");
    }

    #[test]
    fn markdown_fp_filter_keeps_findings_without_line_number() {
        // Findings with no resolvable line are kept (safe default).
        let issues = vec![ReviewIssue {
            file: "doc.md".to_string(),
            line: None,
            severity: Severity::Info,
            issue_type: None,
            title: "vague".to_string(),
            body: "...".to_string(),
            suggested_fix: None,
        }];

        let result = apply_markdown_code_block_filter(issues, &[]);
        assert_eq!(result.len(), 1);
    }
}
