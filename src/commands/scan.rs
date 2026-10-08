use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::config::schema::Config;
use crate::engine::review::apply_ignore_rules;
use crate::engine::review_store;
use crate::engine::scanner::{
    FileEntry, batch_files, files_as_chunks, format_batch_for_prompt, walk_project,
};
use crate::engine::types::TokenUsage;
use crate::engine::{ReviewIssue, inline_suppress, rules, secrets_scanner, security_scanner};
use crate::formatters::{OutputFormat, formatter_for};

/// Default maximum files per LLM batch when `--batch-files` is not specified.
/// Lower this to work around provider token limits or rate-limit errors.
const DEFAULT_MAX_FILES_PER_BATCH: usize = 20;

/// Approximate token budget per batch. Used by the scanner to split large file
/// sets into review-sized chunks that fit within typical model context windows.
const DEFAULT_BATCH_TOKEN_BUDGET: usize = 60_000;

/// Scan command options.
pub struct ScanOptions {
    /// Root directory to scan.
    pub path: Option<String>,
    /// Include glob patterns.
    pub include: Vec<String>,
    /// Exclude glob patterns.
    pub exclude: Vec<String>,
    /// Additional file extensions to include.
    pub extensions: Vec<String>,
    /// Only scan files changed since last scan.
    pub incremental: bool,
    /// Focus areas for review (overrides config).
    pub focus: Vec<String>,
    /// Maximum files per LLM batch (0 = use default 20).
    pub batch_files: usize,
    /// Whether to continue scanning when a batch fails to parse.
    /// When true (default), a failed batch is skipped with a warning and the
    /// rest of the scan continues. When false, a failed batch aborts the run.
    pub continue_on_batch_error: bool,
}

/// Execute the scan command.
///
/// Walks the project directory, filters files, batches them, calls the LLM,
/// and formats the output.
#[allow(clippy::too_many_lines)]
pub async fn execute_scan(
    config: &Config,
    llm_config: &crate::engine::LLMConfig,
    opts: &ScanOptions,
    format: OutputFormat,
) -> Result<i32> {
    let root = match &opts.path {
        Some(p) => Path::new(p).to_path_buf(),
        None => std::env::current_dir()?,
    };

    if !root.is_dir() {
        anyhow::bail!("scan path '{}' is not a directory", root.display());
    }

    // Merge include/exclude with config ignore patterns
    let include = opts.include.clone();
    let mut exclude = config.ignore.files.clone();
    exclude.extend(opts.exclude.clone());

    // Merge focus areas: CLI --focus overrides config
    let effective_focus = if opts.focus.is_empty() {
        config.focus.clone()
    } else {
        opts.focus.clone()
    };

    debug!(root = %root.display(), "starting scan");

    // 1. Walk and collect files
    let mut files = walk_project(&root, &include, &exclude, &opts.extensions)?;

    // 1b. Incremental: filter out unchanged files
    if opts.incremental {
        let cache = ScanCache::load()?;
        let before_count = files.len();
        let root_abs = root.canonicalize().unwrap_or_else(|_| root.clone());
        files.retain(|f| {
            let abs_path = root_abs.join(&f.path);
            let Some(hash) = file_content_hash(&abs_path) else {
                return true; // can't read file, rescan it
            };
            match cache.get(&root_abs, &f.path) {
                Some(cached_hash) if cached_hash == hash => {
                    debug!(file = %f.path, "skipping unchanged file (incremental)");
                    false
                }
                _ => true,
            }
        });
        let skipped = before_count - files.len();
        if skipped > 0 {
            eprintln!(
                "  {} skipped (unchanged since last scan)",
                skipped.to_string().dimmed()
            );
        }
    }

    if files.is_empty() {
        eprintln!("{}", "No files to scan.".yellow());
        return Ok(0);
    }

    eprintln!("🔍 {} files to review…", files.len().to_string().cyan());

    // 2. Calculate total lines
    let total_lines: usize = files.iter().map(|f| f.lines).sum();

    // 2b. Run index-powered deterministic scans (unused imports, dead code)
    // These work even without LLM and add findings to the final report.
    let root_abs = root.canonicalize().unwrap_or_else(|_| root.clone());
    // Same merged exclusion set as `cora index` (#521).
    let mut index_skip = config.ignore.files.clone();
    index_skip.extend(config.rules_config.index_skip_files.iter().cloned());
    index_skip.dedup();
    let index_bridge = crate::engine::index_bridge::IndexBridge::open(&root_abs);
    let index_findings = crate::engine::index_scanner::scan_project_index(
        &index_bridge,
        &files,
        config.rules_config.max_findings,
        &index_skip,
    );
    if !index_findings.is_empty() {
        eprintln!(
            "  {} index-based findings (unused imports, dead code)",
            index_findings.len().to_string().cyan()
        );
    }

    // 3. Batch files
    let max_files_per_batch = if opts.batch_files > 0 {
        opts.batch_files
    } else {
        DEFAULT_MAX_FILES_PER_BATCH
    };
    let batches = batch_files(&files, DEFAULT_BATCH_TOKEN_BUDGET, max_files_per_batch);
    debug!(
        batches = batches.len(),
        max_files = max_files_per_batch,
        "batched files"
    );

    // 4. Build brain context once for the entire scan (if use_brain enabled).
    //    Extracts defined symbols from the file list and queries the symbol
    //    index for impact analysis, affected tests, and related patterns.
    let brain_ctx = if config.context_chain.use_brain {
        crate::engine::review::build_scan_brain_context(
            &files,
            config.context_chain.impact_depth,
            &index_bridge,
        )
    } else {
        None
    };

    // 5. Process batches and collect issues
    let mut all_issues = Vec::new();
    let mut total_tokens: Option<TokenUsage> = None;
    let mut skipped_batches: Vec<(usize, Vec<String>, String)> = Vec::new();

    for (batch_idx, batch) in batches.iter().enumerate() {
        let files_content = format_batch_for_prompt(batch);
        let batch_label = if batches.len() > 1 {
            format!(" (batch {}/{})", batch_idx + 1, batches.len())
        } else {
            String::new()
        };

        eprintln!("  Reviewing{batch_label}…");

        match crate::engine::llm::scan_files(
            llm_config,
            &files_content,
            &effective_focus,
            &config.rules,
            &config.response_format,
            None,
            brain_ctx.as_deref(),
        )
        .await
        {
            Ok((issues, _summary, tokens)) => {
                all_issues.extend(issues);
                total_tokens = match (total_tokens, tokens) {
                    (Some(mut acc), Some(t)) => {
                        acc.input_tokens += t.input_tokens;
                        acc.output_tokens += t.output_tokens;
                        acc.estimated_cost_usd += t.estimated_cost_usd;
                        Some(acc)
                    }
                    (None, Some(t)) => Some(t),
                    (acc, None) => acc,
                };
            }
            Err(err) => {
                let file_list: Vec<String> =
                    batch.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
                let err_string = err.to_string();

                // Always log the failure at warn level so it shows even without --verbose.
                tracing::warn!(
                    batch = batch_idx + 1,
                    total_batches = batches.len(),
                    files = ?file_list,
                    error = %err_string,
                    "batch scan failed"
                );

                if !opts.continue_on_batch_error {
                    eprintln!(
                        "  {} batch {}/{}: {}",
                        "failed".red().bold(),
                        batch_idx + 1,
                        batches.len(),
                        err_string
                    );
                    return Err(err.into());
                }

                eprintln!(
                    "  {} batch {}/{} — skipping ({} files): {}",
                    "warn".yellow().bold(),
                    batch_idx + 1,
                    batches.len(),
                    file_list.len(),
                    err_string
                );
                skipped_batches.push((batch_idx + 1, file_list, err_string));
            }
        }
    }

    if !skipped_batches.is_empty() {
        eprintln!(
            "  {} {} of {} batches skipped due to parse failures.",
            skipped_batches.len().to_string().yellow(),
            skipped_batches.len(),
            batches.len()
        );
    }

    // 5. Merge deterministic + index findings with the LLM findings, then
    //    apply ignore.rules and inline cora-ignore markers. Deterministic
    //    findings survive an LLM failure (#595).
    all_issues.extend(index_findings);
    let all_issues = finalize_issues(config, &files, all_issues);
    let issue_count = all_issues.len();
    let min_severity = config.hook.min_severity_level();
    // Ord order is Critical(0) < Major(1) < Minor(2) < Info(3), so "at or above
    // min_severity" means Ord value <= min_severity.
    let should_block = all_issues.iter().any(|i| i.severity <= min_severity);

    let response = crate::engine::ScanResponse {
        issues: all_issues,
        files_scanned: files.len(),
        lines_scanned: total_lines,
        summary: format!(
            "Scanned {} files ({} lines), found {} issues.",
            files.len(),
            total_lines,
            issue_count
        ),
        tokens_used: total_tokens,
        should_block,
    };

    let formatter = formatter_for(format);
    let output = formatter.format_scan(&response)?;
    println!("{output}");

    // 6. Save scan cache for incremental mode
    if opts.incremental {
        let root_abs = root.canonicalize().unwrap_or_else(|_| root.clone());
        let mut cache = ScanCache::load().unwrap_or_default();
        for f in &files {
            let abs_path = root_abs.join(&f.path);
            let Some(hash) = file_content_hash(&abs_path) else {
                continue; // can't read file, skip cache entry
            };
            cache.set(&root_abs, &f.path, &hash);
        }
        cache.save()?;
        debug!(cached = files.len(), "saved scan cache");
    }

    // 7. Save scan findings to cora.db (best-effort)
    {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().to_string().into());
        let branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().to_string().into());
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let record = review_store::ReviewRecord {
            command: "scan",
            project_root: &cwd,
            commit_hash: commit.as_deref(),
            branch: branch.as_deref(),
            summary: &response.summary,
            gate_status: "disabled",
            files_scanned: response.files_scanned,
            lines_scanned: response.lines_scanned,
            should_block: response.should_block,
            tokens: response.tokens_used.as_ref(),
            issues: &response.issues,
        };
        // Best-effort: a history-write failure never fails the run.
        review_store::persist_review_best_effort(&record);
    }

    if response.should_block && config.hook.mode == "block" {
        Ok(2)
    } else {
        Ok(0)
    }
}

/// Run the deterministic scanners (secrets, security) over whole files and
/// merge them with `issues` (LLM + index findings); then drop findings matched
/// by `ignore.rules` and by inline `cora-ignore:` markers. Reuses the review
/// pipeline's scanners, merge, and filters (#595).
fn finalize_issues(
    config: &Config,
    files: &[FileEntry],
    issues: Vec<ReviewIssue>,
) -> Vec<ReviewIssue> {
    let chunks = files_as_chunks(files);
    let max = config.rules_config.max_findings;
    let mut merged = issues;
    for family in [
        secrets_scanner::scan_secrets(&chunks, max),
        security_scanner::scan_security(&chunks, max),
    ] {
        if !family.is_empty() {
            merged = rules::merge_rule_findings(merged, family);
        }
    }
    let merged = apply_ignore_rules(merged, &config.ignore.rules);
    inline_suppress::apply(merged, &chunks)
}

/// Compute a short SHA256 hash of a file's content for incremental scanning.
/// Returns None if the file cannot be read (caller should rescan it).
#[allow(clippy::format_collect)]
fn file_content_hash(path: &std::path::Path) -> Option<String> {
    use sha2::Digest;
    let bytes = std::fs::read(path).ok()?;
    let hash = sha2::Sha256::digest(&bytes);
    // Use first 8 bytes as hex — consistent representation, no truncation
    Some(hash.iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// Cache of file content hashes for incremental scanning.
/// Stored as JSON in ~/.cora/scan-cache.json.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ScanCache {
    /// Key: canonical root path, Value: { `file_path`: hash }
    projects: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl ScanCache {
    fn cache_path() -> anyhow::Result<std::path::PathBuf> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(home.join(".cora").join("scan-cache.json"))
    }

    fn load() -> Result<Self> {
        let path = Self::cache_path()?;
        if !path.is_file() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        serde_json::from_str(&content).context("failed to parse scan cache")
    }

    fn save(&self) -> Result<()> {
        let path = Self::cache_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    fn get(&self, root: &std::path::Path, file: &str) -> Option<String> {
        let root_key = root.to_string_lossy().to_string();
        self.projects.get(&root_key)?.get(file).cloned()
    }

    fn set(&mut self, root: &std::path::Path, file: &str, hash: &str) {
        let root_key = root.to_string_lossy().to_string();
        self.projects
            .entry(root_key)
            .or_default()
            .insert(file.to_string(), hash.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, content: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            content: content.to_string(),
            lines: content.lines().count(),
        }
    }

    fn secret_file(extra: &str) -> FileEntry {
        entry(
            "src/app.py",
            &format!("import os\npassword = \"hunter2hunter2\"{extra}\nprint(1)\n"),
        )
    }

    fn titles(issues: &[ReviewIssue]) -> Vec<&str> {
        issues.iter().map(|i| i.title.as_str()).collect()
    }

    fn secret_title() -> String {
        let out = finalize_issues(&Config::default(), &[secret_file("")], Vec::new());
        out.iter()
            .find(|i| i.line == Some(2))
            .unwrap_or_else(|| panic!("no finding on line 2: {:?}", titles(&out)))
            .title
            .clone()
    }

    #[test]
    fn finds_hardcoded_secret_without_llm() {
        let out = finalize_issues(&Config::default(), &[secret_file("")], Vec::new());
        assert!(
            out.iter()
                .any(|i| i.file == "src/app.py" && i.line == Some(2)),
            "got {:?}",
            titles(&out)
        );
    }

    #[test]
    fn inline_marker_suppresses_secret() {
        let title = secret_title();
        let file = secret_file(&format!("  # cora-ignore: {title}"));
        let out = finalize_issues(&Config::default(), &[file], Vec::new());
        assert!(
            !out.iter().any(|i| i.line == Some(2)),
            "got {:?}",
            titles(&out)
        );
    }

    #[test]
    fn ignore_rules_suppress_secret() {
        let mut config = Config::default();
        config.ignore.rules = vec![secret_title()];
        let out = finalize_issues(&config, &[secret_file("")], Vec::new());
        assert!(
            !out.iter().any(|i| i.line == Some(2)),
            "got {:?}",
            titles(&out)
        );
    }

    #[tokio::test]
    async fn llm_failure_still_reports_deterministic_findings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "password = \"hunter2hunter2\"\n").unwrap();
        // Nothing listens on port 1: the LLM call fails fast, no real network.
        let llm = crate::engine::LLMConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: "test".to_string(),
            timeout: 2,
            ..Default::default()
        };
        let mut config = Config::default();
        config.hook.mode = "block".to_string();
        let opts = ScanOptions {
            path: Some(dir.path().to_string_lossy().to_string()),
            include: vec![],
            exclude: vec![],
            extensions: vec![],
            incremental: false,
            focus: vec![],
            batch_files: 0,
            continue_on_batch_error: true,
        };
        let code = execute_scan(&config, &llm, &opts, OutputFormat::Json)
            .await
            .unwrap();
        assert_eq!(code, 2, "deterministic finding must survive LLM failure");
    }
}
