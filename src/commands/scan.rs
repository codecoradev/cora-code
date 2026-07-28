use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::config::schema::Config;
use crate::engine::db_writer;
use crate::engine::scanner::{batch_files, format_batch_for_prompt, walk_project};
use crate::engine::types::TokenUsage;
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
            println!(
                "  {} skipped (unchanged since last scan)",
                skipped.to_string().dimmed()
            );
        }
    }

    if files.is_empty() {
        println!("{}", "No files to scan.".yellow());
        return Ok(0);
    }

    println!("🔍 {} files to review…", files.len().to_string().cyan());

    // 2. Calculate total lines
    let total_lines: usize = files.iter().map(|f| f.lines).sum();

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

    // 4. Process batches and collect issues
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

        println!("  Reviewing{batch_label}…");

        match crate::engine::llm::scan_files(
            llm_config,
            &files_content,
            &effective_focus,
            &config.rules,
            &config.response_format,
            None,
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

    // 5. Build response and format
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
        let record = db_writer::ReviewRecord {
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
        if db_writer::save_review_to_db(&record).is_none() {
            debug!("Failed to save scan to cora.db");
        }
    }

    if response.should_block && config.hook.mode == "block" {
        Ok(2)
    } else {
        Ok(0)
    }
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
