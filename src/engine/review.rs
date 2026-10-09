use crate::error::CoraError;
use tracing::{debug, instrument};

use crate::config::schema::Config;
use crate::engine::llm;
use crate::engine::postprocess::{Source, postprocess};
use crate::engine::types::{LLMConfig, ReviewResponse};

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
        std::borrow::Cow::Borrowed(diff)
    };

    // Index bridge: one project handle, rooted via resolve_project_root, shared
    // by every index-backed step so a run from a subdirectory agrees with
    // `cora index` (#566).
    let index_bridge = crate::engine::index_bridge::IndexBridge::open_cwd();
    let project_root = if index_bridge.root().as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_default()
    } else {
        index_bridge.root().to_path_buf()
    };

    // All deterministic checks (rules, secrets, security, index scans) on the
    // ORIGINAL diff — no LLM involved. Context = static analysis + claim
    // warning + one block per scanner family.
    let deterministic = crate::engine::deterministic::run(&diff_chunks, config, &index_bridge);
    if !deterministic.claims.suspicious_claims.is_empty() && !config.sanitize_comments {
        debug!(
            claims = deterministic.claims.suspicious_claims.len(),
            "Untrusted verification claims flagged in added comments"
        );
    }
    let combined_context = deterministic.context(static_context.as_deref());

    // Build context chain (cross-file dependency extraction)
    // NOTE: pass ignore.files (e.g. target/**, node_modules/**) so the resolver
    // never injects build-artifact code — not ignore.rules (finding-type strings).
    let context_chain = crate::engine::context::build_context_chain(
        &diff_chunks,
        &config.context_chain,
        &project_root,
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
            &index_bridge,
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
            &crate::progress::StdoutStream,
        )
        .await
        .inspect(|_| println!()) // trailing newline after streamed output
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
            if !deterministic.is_empty() {
                let n_rules = deterministic.rules.len();
                let n_secrets = deterministic.secrets.len();
                let n_security = deterministic.security.len();
                let n_index_unused = deterministic.index_unused.len();
                let n_index_dead = deterministic.index_dead.len();
                let n_index_breaking = deterministic.index_breaking.len();
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
                let all_deterministic = deterministic.merge_into(vec![]);
                let mut fallback = ReviewResponse {
                    issues: all_deterministic,
                    summary: format!(
                        "LLM review failed: {e}. Showing {n_rules} rule + {n_secrets} secrets + {n_security} security + {n_index_unused} unused imports + {n_index_dead} dead code + {n_index_breaking} breaking changes."
                    ),
                    tokens_used: None,
                    should_block: false,
                };
                fallback.issues = postprocess(fallback.issues, &Source::Diff(&diff_chunks), config);
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

    // Merge deterministic findings (rules, secrets, security, index) with LLM issues
    response.issues = deterministic.merge_into(response.issues);

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

    // Shared post-processing (LLM secret FP cross-check, Markdown code blocks,
    // ignore.rules, inline `cora-ignore:`, context-line filter), see
    // `engine::postprocess`.
    response.issues = postprocess(response.issues, &Source::Diff(&diff_chunks), config);

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
    bridge: &crate::engine::index_bridge::IndexBridge,
) -> Option<String> {
    let (conn, project_id) = bridge.parts()?;

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
            crate::index::graph::impact_analysis(conn, project_id, &def.name, impact_depth)
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
            conn, project_id, &def.name, 1, // depth 1 is enough for test detection
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
            crate::index::brain::brain_search(conn, project_id, &format!("test {}", def.name), 3)
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
        if let Ok(results) = crate::index::brain::brain_search(conn, project_id, &def.name, 3) {
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
    bridge: &crate::engine::index_bridge::IndexBridge,
) -> Option<String> {
    let (conn, project_id) = bridge.parts()?;

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
        if let Ok(results) = crate::index::brain::brain_search(conn, project_id, &query, 5) {
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
            crate::index::graph::impact_analysis(conn, project_id, &r.name, impact_depth)
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
        if let Ok(nodes) = crate::index::graph::impact_analysis(conn, project_id, &r.name, 1) {
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
}
