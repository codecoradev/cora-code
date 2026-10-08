use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::Path;

use crate::engine::diff_parser::{DiffHunk, DiffLine, DiffLineType, FileChunk};
use crate::engine::path_match::PathMatcher;
use crate::error::CoraError;
use ignore::WalkBuilder;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing::debug;

/// A file to be scanned, with its relative path and content.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path from the project root.
    pub path: String,
    /// The file content.
    pub content: String,
    /// Number of lines.
    pub lines: usize,
}

/// View scanned files as synthetic "new file" diff chunks (every line added),
/// so the diff-based deterministic scanners and the inline `cora-ignore:`
/// filter can run on a full-file scan unchanged (#595).
pub fn files_as_chunks(files: &[FileEntry]) -> Vec<FileChunk> {
    files
        .iter()
        .filter(|f| !f.content.is_empty())
        .map(|f| {
            let lines: Vec<DiffLine> = f
                .content
                .lines()
                .zip(1u32..)
                .map(|(text, n)| DiffLine {
                    line_type: DiffLineType::Add,
                    content: text.to_string(),
                    old_line_no: None,
                    new_line_no: Some(n),
                })
                .collect();
            let count = u32::try_from(lines.len()).unwrap_or(u32::MAX);
            let language = Path::new(&f.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            FileChunk {
                old_path: None,
                new_path: Some(f.path.clone()),
                language,
                chunks: vec![DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: count,
                    header: String::new(),
                    lines,
                }],
                is_binary: false,
                is_deleted: false,
                is_new: true,
            }
        })
        .collect()
}

/// File extensions to include in scans by default (source code).
const DEFAULT_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "kt", "rb", "c", "cpp", "h", "hpp", "cs",
    "php", "swift", "scala", "vue", "svelte", "sh", "bash", "zsh", "ps1", "toml", "yaml", "yml",
    "json", "sql", "graphql", "proto", "md", "rst", "txt", "html", "css", "scss", "less",
];

/// Walk a project directory, respecting .gitignore, and collect scannable files.
///
/// Uses `include` and `exclude` glob patterns to filter results.
/// The `ignore` crate handles .gitignore (including nested), .git/info/exclude,
/// and hidden-file filtering automatically.
#[allow(clippy::unnecessary_wraps, clippy::format_push_string)]
pub fn walk_project(
    root: &Path,
    include_patterns: &[String],
    exclude_patterns: &[String],
    extra_extensions: &[String],
) -> std::result::Result<Vec<FileEntry>, CoraError> {
    debug!(
        root = %root.display(),
        "walking project directory"
    );

    // Build extension set
    let mut extensions: BTreeSet<String> = DEFAULT_EXTENSIONS
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    for ext in extra_extensions {
        extensions.insert(ext.trim_start_matches('.').to_lowercase());
    }

    let include_globs = PathMatcher::new(include_patterns);
    let exclude_globs = PathMatcher::new(exclude_patterns);

    let mut entries = Vec::new();

    let spinner = ProgressBar::new_spinner();
    // Hide spinner when stderr is not a terminal (piped/redirected)
    if std::io::stderr().is_terminal() {
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        spinner.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ "),
        );
        spinner.set_message("Scanning files…");
    } else {
        spinner.set_draw_target(ProgressDrawTarget::hidden());
    }

    let walker = WalkBuilder::new(root)
        .hidden(true) // respect hidden files (skip dotfiles)
        .git_ignore(true) // respect .gitignore at all levels
        .git_global(true) // respect core.excludesFile
        .git_exclude(true) // respect .git/info/exclude
        .require_git(false) // apply gitignore rules even outside git repos
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                debug!(error = %err, "error during directory walk");
                continue;
            }
        };

        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Get relative path (entry.depth() ensures it's under root)
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Check exclude patterns
        if exclude_globs.is_match(&relative) {
            continue;
        }

        // Check include patterns (if any specified)
        if !include_globs.is_empty() && !include_globs.is_match(&relative) {
            continue;
        }

        // Check extension
        let has_extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| extensions.contains(&e.to_lowercase()));
        if has_extension == Some(false) {
            continue;
        }

        // Read file content (skip binary / unreadable)
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                debug!(file = %relative, error = %e, "skipping unreadable file");
                continue;
            }
        };

        // Skip empty files
        if content.trim().is_empty() {
            continue;
        }

        // Skip very large files (> 200KB)
        if content.len() > 200_000 {
            debug!(file = %relative, "skipping large file");
            continue;
        }

        let lines = content.lines().count();
        entries.push(FileEntry {
            path: relative,
            content,
            lines,
        });
    }

    spinner.finish_and_clear();
    debug!(files = entries.len(), "found scannable files");

    Ok(entries)
}

/// Split files into batches to avoid exceeding LLM token limits.
pub fn batch_files(files: &[FileEntry], max_chars: usize, max_files: usize) -> Vec<Vec<FileEntry>> {
    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_size: usize = 0;

    for file in files {
        let file_size = file.content.len() + file.path.len() + 20; // + header overhead

        if (current_batch.len() >= max_files || current_size + file_size > max_chars)
            && !current_batch.is_empty()
        {
            batches.push(std::mem::take(&mut current_batch));
            current_size = 0;
        }

        current_size += file_size;
        current_batch.push(file.clone());
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

/// Format a batch of files for the LLM prompt.
#[allow(clippy::format_push_string)]
pub fn format_batch_for_prompt(files: &[FileEntry]) -> String {
    let mut output = String::new();

    for file in files {
        output.push_str(&format!("=== {} ===\n", file.path));
        for (i, line) in file.content.lines().enumerate() {
            output.push_str(&format!("{:>5} | {}\n", i + 1, line));
        }
        output.push('\n');
    }

    output
}
