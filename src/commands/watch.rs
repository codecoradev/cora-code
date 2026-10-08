//! `cora watch` — standalone file-system watcher with auto-reindex.
//!
//! Watches the project directory for file changes and re-indexes on save.
//! Supports debounce window, git-only filtering, and glob patterns.
//!
//! Each poll compares an `(mtime, size)` snapshot of the watched files with
//! the previous one; only new/modified files are re-indexed (deletions are
//! pruned by the same run), and idle ticks do no indexing work.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use colored::Colorize;

use crate::engine::path_match::{PathMatcher, PathPattern};
use crate::index::session::IndexSession;

/// Entry point for `cora watch` (also backs `cora index --watch`).
///
/// Runs an initial index, then polls for changes at the debounce interval.
/// On each poll cycle, re-indexes only the files that changed since the last
/// snapshot and reports updated files/symbols.
/// Config, backend, skip patterns and root all come from the [`IndexSession`].
///
/// # Arguments
/// * `session` - configured index session (owns root, DB, skip patterns)
/// * `debounce_ms` - Minimum time between reindex cycles (default 500ms)
/// * `git_only` - If true, only process files tracked by git
/// * `filter` - Optional glob pattern (e.g. `src/**/*.rs`); restricts which
///   files are watched and re-indexed
/// * `verbose` - Verbose output
pub fn run_watch(
    session: &IndexSession,
    debounce_ms: u64,
    git_only: bool,
    filter: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let project_root = session.root();

    // Build git-tracked file set if --git-only
    let git_files: Option<HashSet<PathBuf>> = if git_only {
        Some(get_git_tracked_files(project_root)?)
    } else {
        None
    };

    // Compile glob filter if provided
    let glob_matcher = filter.map(|p| {
        PathPattern::new(p).unwrap_or_else(|e| {
            eprintln!("{} Invalid glob pattern '{p}': {e}", "⚠ ".yellow());
            std::process::exit(1);
        })
    });

    // Same exclusions as the indexer (ignore.files / index_skip_files)
    let skip = session.skip_patterns().map(PathMatcher::new);
    let scan = || {
        scan_files(
            project_root,
            &git_files,
            glob_matcher.as_ref(),
            skip.as_ref(),
        )
    };

    let debounce = Duration::from_millis(debounce_ms);

    // Baseline snapshot, taken before the initial index so an edit racing
    // with it still shows up as a change on the first poll.
    let mut snapshot = scan()?;

    // Initial index
    eprintln!("{}", "🔍 Initial index...".cyan());
    let stats = session.index(verbose)?;
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

        // Compare the tree against the last-seen snapshot
        let current = scan()?;
        let changes = diff_snapshots(&snapshot, &current);
        snapshot = current;
        if changes.is_empty() {
            continue;
        }

        last_reindex = now;

        if verbose {
            eprintln!(
                "{}",
                format!(
                    "Changed files: {} ({} deleted)",
                    changes.changed.len(),
                    changes.deleted.len()
                )
                .dimmed()
            );
        }

        // Re-index only the changed files; deletions are pruned by the same run.
        let changed_rel: HashSet<String> = changes
            .changed
            .iter()
            .map(|p| rel_string(project_root, p))
            .collect();
        let stats = session.index_matching(verbose, &|rel| changed_rel.contains(rel))?;

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

/// `(mtime in ns, size)` per watched file, as last seen on disk.
type Snapshot = HashMap<PathBuf, (u128, u64)>;

/// Files that differ between two snapshots.
#[derive(Debug, Default)]
struct ChangeSet {
    /// New or modified files.
    changed: Vec<PathBuf>,
    /// Files present in the old snapshot but gone now.
    deleted: Vec<PathBuf>,
}

impl ChangeSet {
    fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.deleted.is_empty()
    }
}

/// Root-relative path in the form the indexer stores.
fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// New or (mtime,size)-different files are `changed`; files missing from
/// `new` are `deleted`.
fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> ChangeSet {
    let mut set = ChangeSet::default();
    for (path, sig) in new {
        if old.get(path) != Some(sig) {
            set.changed.push(path.clone());
        }
    }
    for path in old.keys() {
        if !new.contains_key(path) {
            set.deleted.push(path.clone());
        }
    }
    set.changed.sort();
    set.deleted.sort();
    set
}

/// Snapshot every watched file: source extension, git-only, `--filter` glob
/// and the session's skip patterns (same exclusions as the indexer).
fn scan_files(
    project_root: &Path,
    git_files: &Option<HashSet<PathBuf>>,
    glob_matcher: Option<&PathPattern>,
    skip: Option<&PathMatcher>,
) -> Result<Snapshot> {
    let mut snapshot = Snapshot::new();
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
            if !pattern.matches(&rel.to_string_lossy().replace('\\', "/")) {
                return;
            }
        }

        // Config skip patterns (ignore.files / index_skip_files)
        if skip.is_some_and(|m| m.is_match(&rel.to_string_lossy())) {
            return;
        }

        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        snapshot.insert(path.to_path_buf(), (mtime, meta.len()));
    };
    walk_files(project_root, &mut walker)?;

    Ok(snapshot)
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
    }

    #[test]
    fn test_get_git_tracked_files_no_repo() {
        let tmp = TempDir::new().unwrap();
        let result = get_git_tracked_files(tmp.path());
        // Should fail gracefully (no git repo)
        assert!(result.is_err() || result.unwrap().is_empty());
    }

    fn scan(root: &Path) -> Snapshot {
        scan_files(root, &None, None, None).unwrap()
    }

    /// Rewrite a file with a clearly different mtime and size.
    fn touch_modify(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        let t = std::time::SystemTime::now() + Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn empty_dir_has_no_changes() {
        let tmp = TempDir::new().unwrap();
        let snap = scan(tmp.path());
        assert!(snap.is_empty());
        assert!(diff_snapshots(&snap, &scan(tmp.path())).is_empty());
    }

    #[test]
    fn filters_non_source() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("README.md"), "# readme").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let snap = scan(root);
        assert!(snap.keys().any(|p| p.ends_with("main.rs")));
        assert!(!snap.keys().any(|p| p.ends_with("README.md")));
    }

    #[test]
    fn unchanged_tree_is_empty_change_set() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.rs"), "fn a() {}").unwrap();
        // The first scan is the baseline: a non-empty tree is not "all changed".
        let baseline = scan(tmp.path());
        assert!(!baseline.is_empty());
        assert!(diff_snapshots(&baseline, &scan(tmp.path())).is_empty());
    }

    #[test]
    fn modified_file_is_the_only_change() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("b.rs"), "fn b() {}").unwrap();
        let before = scan(root);
        touch_modify(&root.join("b.rs"), "fn b() {}\nfn b2() {}\n");
        let d = diff_snapshots(&before, &scan(root));
        assert_eq!(d.changed, vec![root.join("b.rs")]);
        assert!(d.deleted.is_empty());
    }

    #[test]
    fn new_and_deleted_files_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("b.rs"), "fn b() {}").unwrap();
        let before = scan(root);
        fs::write(root.join("c.rs"), "fn c() {}").unwrap();
        fs::remove_file(root.join("b.rs")).unwrap();
        let d = diff_snapshots(&before, &scan(root));
        assert_eq!(d.changed, vec![root.join("c.rs")]);
        assert_eq!(d.deleted, vec![root.join("b.rs")]);
    }

    #[test]
    fn glob_filter_ignores_non_matching_changes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}").unwrap();
        fs::write(root.join("src/b.rs"), "fn b() {}").unwrap();
        let pat = PathPattern::new("a.rs").unwrap();
        let s = || scan_files(root, &None, Some(&pat), None).unwrap();
        let before = s();
        assert_eq!(before.len(), 1);
        touch_modify(&root.join("src/b.rs"), "fn b() {}\nfn b2() {}\n");
        assert!(diff_snapshots(&before, &s()).is_empty());
        touch_modify(&root.join("src/a.rs"), "fn a() {}\nfn a2() {}\n");
        let d = diff_snapshots(&before, &s());
        assert_eq!(d.changed, vec![root.join("src/a.rs")]);
    }

    #[test]
    fn skip_patterns_are_respected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("gen")).unwrap();
        fs::write(root.join("gen/x.rs"), "fn x() {}").unwrap();
        fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        let skip = PathMatcher::new(&["gen/**".to_string()]);
        let before = scan_files(root, &None, None, Some(&skip)).unwrap();
        assert_eq!(before.len(), 1);
        touch_modify(&root.join("gen/x.rs"), "fn x() {}\nfn y() {}\n");
        let after = scan_files(root, &None, None, Some(&skip)).unwrap();
        assert!(diff_snapshots(&before, &after).is_empty());
    }

    /// End to end: a non-matching change yields no change set (the loop never
    /// indexes); a matching one reindexes exactly that file; deletions prune.
    #[test]
    fn filtered_reindex_touches_only_matching_changed_files() {
        use crate::index::session::{ConfigSource, IndexSession};
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").unwrap();
        fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").unwrap();
        let session = IndexSession::open_at(root, ConfigSource::ProjectOnlyAt(root)).unwrap();
        session.index(false).unwrap();
        let pat = PathPattern::new("a.rs").unwrap();
        let s = || scan_files(root, &None, Some(&pat), None).unwrap();
        let snap = s();

        // b.rs changes: nothing detected.
        touch_modify(&root.join("src/b.rs"), "pub fn beta() {}\npub fn b2() {}\n");
        let cur = s();
        assert!(diff_snapshots(&snap, &cur).is_empty());
        // Even a forced run with an empty include set leaves b.rs stale.
        let st = session.index_matching(false, &|_| false).unwrap();
        assert_eq!(st.files_indexed, 0);

        // a.rs changes: exactly one file reindexed.
        touch_modify(
            &root.join("src/a.rs"),
            "pub fn alpha() {}\npub fn a2() {}\n",
        );
        let next = s();
        let d = diff_snapshots(&cur, &next);
        let rel: HashSet<String> = d.changed.iter().map(|p| rel_string(root, p)).collect();
        let st = session.index_matching(false, &|r| rel.contains(r)).unwrap();
        assert_eq!(st.files_indexed, 1);
        assert_eq!(st.files_pruned, 0);
        assert_eq!(session.summary().unwrap().total_files, 2);

        // Deleting a.rs prunes only it.
        fs::remove_file(root.join("src/a.rs")).unwrap();
        let d = diff_snapshots(&next, &s());
        assert_eq!(d.deleted.len(), 1);
        let st = session.index_matching(false, &|_| false).unwrap();
        assert_eq!(st.files_pruned, 1);
        assert_eq!(session.summary().unwrap().total_files, 1);
    }
}
