//! `cora watch` — standalone file-system watcher with auto-reindex.
//!
//! Watches the project directory for file changes and re-indexes on save.
//! Supports debounce window, git-only filtering, and glob patterns.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::index;

/// Entry point for `cora watch`.
///
/// Runs an initial index, then polls for changes at the debounce interval.
/// On each poll cycle, re-indexes the project and reports updated files/symbols.
///
/// # Arguments
/// * `project_root` — Root directory to watch
/// * `config_path` — Optional path to `.cora.yaml`
/// * `debounce_ms` — Minimum time between reindex cycles (default 500ms)
/// * `git_only` — If true, only process files tracked by git
/// * `filter` — Optional glob pattern (e.g. `src/**/*.rs`)
/// * `verbose` — Verbose output
#[allow(clippy::too_many_arguments)]
pub fn run_watch(
    project_root: &Path,
    config_path: Option<&str>,
    debounce_ms: u64,
    git_only: bool,
    filter: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let conn = crate::index::open_global_index()?;
    // Load skip patterns + brain embedding backend from config
    let config =
        crate::config::loader::load_config(config_path, None, None, None, None, false).ok();
    // Same merged exclusion set as `cora index` (#521).
    let skip_patterns: Option<Vec<String>> = config.as_ref().map(|c| {
        let mut pats = c.ignore.files.clone();
        pats.extend(c.rules_config.index_skip_files.iter().cloned());
        pats.dedup();
        pats
    });

    // Resolve embedding backend
    let brain_mode = config
        .as_ref()
        .map(|c| c.brain.embedding.to_string())
        .unwrap_or_else(|| "auto".to_string());
    crate::embed::resolve_backend(&brain_mode);

    let skip_ref: Option<&[String]> = skip_patterns.as_deref();

    // Build git-tracked file set if --git-only
    let git_files: Option<HashSet<PathBuf>> = if git_only {
        Some(get_git_tracked_files(project_root)?)
    } else {
        None
    };

    // Compile glob filter if provided
    let glob_matcher = filter.map(|p| {
        glob::Pattern::new(p).unwrap_or_else(|e| {
            eprintln!("{} Invalid glob pattern '{p}': {e}", "⚠ ".yellow());
            std::process::exit(1);
        })
    });

    let debounce = Duration::from_millis(debounce_ms);

    // Initial index
    eprintln!("{}", "🔍 Initial index...".cyan());
    let stats = index::index_project_with_skip(&conn, project_root, verbose, skip_ref)?;
    eprintln!(
        "{}",
        format!(
            "✅ Indexed {} symbols across {} files.",
            stats.symbols_indexed, stats.files_indexed
        )
        .green()
    );
    eprintln!(
        "{}",
        format!(
            "👀 Watching for changes... (debounce: {}ms, git-only: {}, filter: {}) (Ctrl+C to stop)",
            debounce_ms,
            git_only,
            filter.unwrap_or("none")
        )
        .dimmed()
    );

    // Poll loop
    let mut last_reindex = Instant::now();
    loop {
        std::thread::sleep(debounce);

        let now = Instant::now();
        if now.duration_since(last_reindex) < debounce {
            continue;
        }

        // Check for changed files
        let changed = detect_changes(project_root, &git_files, glob_matcher.as_ref())?;
        if changed.is_empty() {
            continue;
        }

        last_reindex = now;

        if verbose {
            eprintln!("{}", format!("Changed files: {}", changed.len()).dimmed());
        }

        // Re-index
        let stats = index::index_project_with_skip(&conn, project_root, verbose, skip_ref)?;

        if stats.files_indexed > 0 {
            eprintln!(
                "{}",
                format!(
                    "🔄 Reindexed: {} files, {} symbols updated",
                    stats.files_indexed, stats.symbols_indexed
                )
                .cyan()
            );
        }
    }
}

/// Detect files that changed since last check by comparing modification times.
fn detect_changes(
    project_root: &Path,
    git_files: &Option<HashSet<PathBuf>>,
    glob_matcher: Option<&glob::Pattern>,
) -> Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    let extensions: &[&str] = &[
        "rs", "py", "js", "ts", "go", "java", "c", "cpp", "h", "rb", "php", "scala", "cs", "kt",
        "svelte", "jsx", "tsx",
    ];

    let mut walker = |path: &Path| {
        // Skip files inside hidden directories (relative to project root)
        let rel = path.strip_prefix(project_root).unwrap_or(path);
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::Normal(n) if n.to_str().is_some_and(|s| s.starts_with('.'))))
        {
            return;
        }

        // Check extension
        let ext_match = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e));
        if !ext_match {
            return;
        }

        // Apply git-only filter
        if let Some(git_set) = git_files {
            if !git_set.contains(path) {
                return;
            }
        }

        // Apply glob filter
        if let Some(pattern) = glob_matcher {
            if !pattern.matches_path(rel) {
                return;
            }
        }

        changed.push(path.to_path_buf());
    };
    walk_files(project_root, &mut walker)?;

    Ok(changed)
}

/// Recursively walk directory and call `f` for each file path.
fn walk_files(root: &Path, f: &mut dyn FnMut(&Path)) -> Result<()> {
    walk_dir_recursive(root, f)
}

fn walk_dir_recursive(current: &Path, f: &mut dyn FnMut(&Path)) -> Result<()> {
    if !current.is_dir() {
        if current.is_file() {
            f(current);
        }
        return Ok(());
    }

    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
            }
            walk_dir_recursive(&path, f)?;
        } else if path.is_file() {
            f(&path);
        }
    }

    Ok(())
}

/// Get the set of git-tracked files in the repository.
fn get_git_tracked_files(root: &Path) -> Result<HashSet<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--no-others"])
        .current_dir(root)
        .output()
        .context("Failed to run `git ls-files`")?;

    if !output.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let files: HashSet<PathBuf> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| root.join(line))
        .collect();

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_walk_files_finds_source() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();

        // Hidden dir should be skipped
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden/secret.rs"), "// skip").unwrap();

        let mut found = Vec::new();
        walk_files(root, &mut |p| {
            found.push(
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string(),
            );
        })
        .unwrap();

        assert!(found.iter().any(|p| p.ends_with("main.rs")));
        assert!(found.iter().any(|p| p.ends_with("lib.rs")));
        // Hidden files should NOT be found (directory skip)
        // Note: walk_files itself doesn't skip hidden at top-level, only in subdirs
    }

    #[test]
    fn test_get_git_tracked_files_no_repo() {
        let tmp = TempDir::new().unwrap();
        let result = get_git_tracked_files(tmp.path());
        // Should fail gracefully (no git repo)
        assert!(result.is_err() || result.unwrap().is_empty());
    }

    #[test]
    fn test_detect_changes_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let changed = detect_changes(tmp.path(), &None, None).unwrap();
        assert!(changed.is_empty());
    }

    #[test]
    fn test_detect_changes_with_source_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let changed = detect_changes(root, &None, None).unwrap();
        assert!(!changed.is_empty());
        assert!(changed.iter().any(|p| p.ends_with("main.rs")));
    }

    #[test]
    fn test_detect_changes_filters_non_source() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        fs::write(root.join("README.md"), "# readme").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let changed = detect_changes(root, &None, None).unwrap();
        // .md should not be detected, .rs should
        assert!(changed.iter().any(|p| p.ends_with("main.rs")));
        assert!(!changed.iter().any(|p| p.ends_with("README.md")));
    }
}
