use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::config::schema::Config;
use crate::engine::ReviewIssue;
use crate::engine::finalize::{self, HistoryEntry, ShapeOptions};
use crate::engine::judge::{
    DETERMINISTIC_ONLY, HttpScanJudge, JudgeChoice, ScanBatch, ScanJudge, admit,
};
use crate::engine::postprocess::{Source, dropped_warning, postprocess_report};
use crate::engine::scanner::{FileEntry, batch_files, format_batch_for_prompt, walk_project};
use crate::engine::types::TokenUsage;
use crate::formatters::{OutputFormat, formatter_for};

/// Default maximum files per LLM batch when `--batch-files` is not specified.
/// Lower this to work around provider token limits or rate-limit errors.
pub(crate) const DEFAULT_MAX_FILES_PER_BATCH: usize = 20;

/// Character budget per batch (roughly 4 characters per token). Used by the
/// scanner to split large file sets into review-sized chunks that fit within
/// typical model context windows.
pub(crate) const DEFAULT_BATCH_CHAR_BUDGET: usize = 60_000;

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
    judge: &JudgeChoice,
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
    let index_bridge = crate::engine::index_bridge::IndexBridge::open(&root_abs);
    let index_findings = index_findings(config, &index_bridge, &files);
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
    let batches = batch_files(&files, DEFAULT_BATCH_CHAR_BUDGET, max_files_per_batch);
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

    // 5. Judge the batches and collect issues (no LLM: deterministic only)
    let BatchRun {
        issues: mut all_issues,
        tokens: total_tokens,
    } = match judge {
        JudgeChoice::Llm(llm_config) => {
            judge_batches(
                &HttpScanJudge { llm_config },
                config,
                &batches,
                &effective_focus,
                brain_ctx.as_deref(),
                opts.continue_on_batch_error,
            )
            .await?
        }
        JudgeChoice::Deterministic => BatchRun {
            issues: Vec::new(),
            tokens: None,
        },
    };

    // 5. Merge deterministic + index findings with the LLM findings, then
    //    apply ignore.rules and inline cora-ignore markers. Deterministic
    //    findings survive an LLM failure (#595).
    all_issues.extend(index_findings);
    let (mut all_issues, dropped) = finalize_issues(config, &files, all_issues);
    if let Some(w) = dropped_warning(dropped, config) {
        eprintln!("{}", w.yellow());
    }
    let issue_count = all_issues.len();
    // Scan lists every finding; only `hook.min_severity` decides blocking.
    let shaped = finalize::shape_issues(
        &mut all_issues,
        config,
        &ShapeOptions {
            min_severity: config.hook.min_severity_level(),
            filter: false,
            gate: false,
            ci: false,
        },
    );
    let should_block = shaped.should_block;

    // A clean deterministic result is not an AI scan: say so in the summary.
    let no_judge_note = if matches!(judge, JudgeChoice::Deterministic) {
        format!(" {DETERMINISTIC_ONLY}")
    } else {
        String::new()
    };

    let response = crate::engine::ScanResponse {
        issues: all_issues,
        files_scanned: files.len(),
        lines_scanned: total_lines,
        summary: format!(
            "Scanned {} files ({} lines), found {} issues{}.",
            files.len(),
            total_lines,
            issue_count,
            if dropped > 0 {
                format!(
                    " ({dropped} more deterministic findings not shown, rules.max_findings cap)"
                )
            } else {
                String::new()
            }
        ) + &no_judge_note,
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
    finalize::save_history(
        &HistoryEntry {
            command: "scan",
            summary: &response.summary,
            files_scanned: response.files_scanned,
            lines_scanned: response.lines_scanned,
            tokens: response.tokens_used.as_ref(),
        },
        &response.issues,
        &shaped,
    );

    Ok(shaped.exit_code)
}

/// Index-backed deterministic findings (unused imports, dead code) for `files`.
///
/// Uncapped: the cap is enforced after suppression in `postprocess` (#624).
/// Uses the same merged exclusion set as `cora index` (#521).
pub(crate) fn index_findings(
    config: &Config,
    bridge: &crate::engine::index_bridge::IndexBridge,
    files: &[FileEntry],
) -> Vec<ReviewIssue> {
    let mut index_skip = config.ignore.files.clone();
    index_skip.extend(config.rules_config.index_skip_files.iter().cloned());
    index_skip.dedup();
    crate::engine::index_scanner::scan_project_index(bridge, files, usize::MAX, &index_skip)
}

/// Findings and token usage gathered from all scan batches.
struct BatchRun {
    issues: Vec<ReviewIssue>,
    tokens: Option<TokenUsage>,
}

/// Ask `judge` about every batch. A batch that fails is skipped with a warning
/// (`continue_on_error`) or aborts the scan. Every batch's answer passes
/// [`admit`] first: it is untrusted (#644).
async fn judge_batches<J: ScanJudge>(
    judge: &J,
    config: &Config,
    batches: &[Vec<FileEntry>],
    focus: &[String],
    brain_context: Option<&str>,
    continue_on_error: bool,
) -> Result<BatchRun> {
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

        match judge
            .judge_batch(config, &files_content, focus, brain_context)
            .await
        {
            Ok(ScanBatch { issues, tokens }) => {
                // Only files of this batch, bounded text, no deterministic markers.
                let batch_paths: Vec<String> = batch.iter().map(|f| f.path.clone()).collect();
                all_issues.extend(admit(issues, &batch_paths, &Source::Files(batch)));
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
                let file_list: Vec<String> = batch.iter().map(|f| f.path.clone()).collect();
                let err_string = err.to_string();

                // Always log the failure at warn level so it shows even without --verbose.
                tracing::warn!(
                    batch = batch_idx + 1,
                    total_batches = batches.len(),
                    files = ?file_list,
                    error = %err_string,
                    "batch scan failed"
                );

                if !continue_on_error {
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

    Ok(BatchRun {
        issues: all_issues,
        tokens: total_tokens,
    })
}

/// Run the deterministic scanners (rules, secrets, security) over whole files and
/// merge them with `issues` (LLM + index findings); then drop findings matched
/// by `ignore.rules` and by inline `cora-ignore:` markers. Reuses the review
/// pipeline's scanners, merge, and filters (#595).
pub(crate) fn finalize_issues(
    config: &Config,
    files: &[FileEntry],
    issues: Vec<ReviewIssue>,
) -> (Vec<ReviewIssue>, usize) {
    let report = postprocess_report(issues, &Source::Files(files), config);
    (report.issues, report.dropped)
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
        let out = finalize_issues(&Config::default(), &[secret_file("")], Vec::new()).0;
        out.iter()
            .find(|i| i.line == Some(2))
            .unwrap_or_else(|| panic!("no finding on line 2: {:?}", titles(&out)))
            .title
            .clone()
    }

    #[test]
    fn finds_hardcoded_secret_without_llm() {
        let out = finalize_issues(&Config::default(), &[secret_file("")], Vec::new()).0;
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
        let out = finalize_issues(&Config::default(), &[file], Vec::new()).0;
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
        let out = finalize_issues(&config, &[secret_file("")], Vec::new()).0;
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
        let code = execute_scan(&config, &JudgeChoice::Llm(llm), &opts, OutputFormat::Json)
            .await
            .unwrap();
        assert_eq!(code, 2, "deterministic finding must survive LLM failure");
    }

    // ─── #645: scan runs the rules engine, and has a judge seam ───

    #[test]
    fn builtin_rules_fire_in_scan() {
        let file = entry("src/lib.rs", "fn main() {\n    // TODO: handle this\n}\n");
        let out = finalize_issues(&Config::default(), &[file], Vec::new()).0;
        assert!(
            out.iter().any(|i| i.rule_id.as_deref() == Some("bug-todo")),
            "rules engine must run in scan, got {:?}",
            titles(&out)
        );
    }

    #[test]
    fn disabled_rules_engine_stays_off_in_scan() {
        let mut config = Config::default();
        config.rules_config.enabled = false;
        let file = entry("src/lib.rs", "// TODO: later\n");
        let out = finalize_issues(&config, &[file], Vec::new()).0;
        assert!(!out.iter().any(|i| i.rule_id.as_deref() == Some("bug-todo")));
    }

    struct FakeScanJudge(Result<Vec<ReviewIssue>, String>);

    impl ScanJudge for FakeScanJudge {
        async fn judge_batch(
            &self,
            _config: &Config,
            _files_content: &str,
            _focus: &[String],
            _brain_context: Option<&str>,
        ) -> std::result::Result<ScanBatch, crate::error::CoraError> {
            match &self.0 {
                Ok(issues) => Ok(ScanBatch {
                    issues: issues.clone(),
                    tokens: None,
                }),
                Err(msg) => Err(crate::error::CoraError::LlmParse(msg.clone())),
            }
        }
    }

    fn judged(file: &str, line: u32) -> ReviewIssue {
        ReviewIssue::new(
            file,
            Some(line),
            crate::engine::Severity::Major,
            "judge finding",
        )
        .with_type("bug")
        .with_body("body")
    }

    fn one_batch() -> Vec<Vec<FileEntry>> {
        vec![vec![entry("a.rs", "fn a() {}\nfn b() {}\n")]]
    }

    #[tokio::test]
    async fn judge_findings_for_the_batch_are_admitted() {
        let judge = FakeScanJudge(Ok(vec![judged("a.rs", 1), judged("elsewhere.rs", 1)]));
        let run = judge_batches(&judge, &Config::default(), &one_batch(), &[], None, true)
            .await
            .unwrap();
        assert_eq!(run.issues.len(), 1, "foreign file must be dropped");
        assert_eq!(run.issues[0].file, "a.rs");
    }

    #[tokio::test]
    async fn failed_batch_is_skipped_when_continuing() {
        let judge = FakeScanJudge(Err("bad json".to_string()));
        let run = judge_batches(&judge, &Config::default(), &one_batch(), &[], None, true)
            .await
            .unwrap();
        assert!(run.issues.is_empty());
    }

    #[tokio::test]
    async fn failed_batch_aborts_without_continue() {
        let judge = FakeScanJudge(Err("bad json".to_string()));
        let err = judge_batches(&judge, &Config::default(), &one_batch(), &[], None, false).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn no_judge_scans_deterministically_only() {
        let run = judge_batches(
            &crate::engine::judge::NoJudge,
            &Config::default(),
            &one_batch(),
            &[],
            None,
            true,
        )
        .await
        .unwrap();
        assert!(run.issues.is_empty());
    }

    #[tokio::test]
    async fn deterministic_scan_needs_no_llm_and_still_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "password = \"hunter2hunter2\"\n").unwrap();
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
        let code = execute_scan(
            &config,
            &JudgeChoice::Deterministic,
            &opts,
            OutputFormat::Json,
        )
        .await
        .unwrap();
        assert_eq!(code, 2);
    }
}
