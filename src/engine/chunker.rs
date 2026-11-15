//! Diff chunker — splits large diffs into review-sized chunks.
//!
//! When a diff exceeds the request budget (`review.max_chunk_chars`), this
//! module splits it into smaller chunks (grouped by file) that each fit within
//! the budget. Each chunk is a valid unified diff that can be reviewed
//! independently. A single file larger than the budget is split at hunk
//! boundaries, repeating the file header in every piece.
//!
//! Splitting is lossless: the chunks together contain every byte of the input
//! (#720). Paths matched by `review.exclude` are removed by [`exclude_paths`]
//! before chunking, and reported to the caller.

use crate::engine::path_match::PathMatcher;
use tracing::debug;

/// A chunk of diff ready for review.
#[derive(Debug, Clone)]
pub struct DiffChunk {
    /// Chunk index (0-based).
    pub index: usize,
    /// Total number of chunks.
    pub total: usize,
    /// Label for progress reporting (e.g., "src/config/*").
    pub label: String,
    /// The unified diff text for this chunk.
    pub diff: String,
    /// Files included in this chunk.
    pub file_count: usize,
    /// Paths of the files in this chunk (a file split by hunk appears in
    /// each of its pieces).
    pub files: Vec<String>,
}

/// Result of [`exclude_paths`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Excluded {
    /// The diff without the excluded files.
    pub diff: String,
    /// Paths removed from the diff, in diff order.
    pub excluded: Vec<String>,
}

/// Remove every file whose path matches `matcher` from `diff`.
///
/// The remaining files keep their exact bytes. With an empty matcher the diff
/// is returned unchanged.
pub fn exclude_paths(diff: &str, matcher: &PathMatcher) -> Excluded {
    if matcher.is_empty() {
        return Excluded {
            diff: diff.to_string(),
            excluded: Vec::new(),
        };
    }
    let mut kept = String::with_capacity(diff.len());
    let mut excluded = Vec::new();
    for fd in split_by_file(diff) {
        if matcher.is_match(&fd.path) {
            excluded.push(fd.path);
        } else {
            kept.push_str(&fd.diff);
        }
    }
    Excluded {
        diff: kept,
        excluded,
    }
}

/// Split a large diff into chunks that each fit within `max_size` bytes.
///
/// Grouping strategy: files are packed in diff order. A file larger than
/// `max_size` is split at hunk boundaries; a single hunk larger than
/// `max_size` still gets a chunk of its own.
pub fn chunk_diff(diff: &str, max_size: usize) -> Vec<DiffChunk> {
    let file_diffs: Vec<FileDiff> = split_by_file(diff)
        .into_iter()
        .flat_map(|fd| split_oversize(fd, max_size))
        .collect();

    if file_diffs.is_empty() {
        return vec![DiffChunk {
            index: 0,
            total: 1,
            label: "empty".to_string(),
            diff: diff.to_string(),
            file_count: 0,
            files: Vec::new(),
        }];
    }

    let groups = group_by_size(&file_diffs, max_size);

    let total = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(|(i, group)| {
            let label = derive_label(&group);
            let combined_diff = group.iter().map(|f| f.diff.as_str()).collect::<String>();
            let files: Vec<String> = group.iter().map(|f| f.path.clone()).collect();
            DiffChunk {
                index: i,
                total,
                label,
                diff: combined_diff,
                file_count: files.len(),
                files,
            }
        })
        .collect()
}

/// A single file's diff block with metadata.
#[derive(Debug, Clone)]
struct FileDiff {
    /// Path of the file (new path, or old path for a deletion).
    path: String,
    /// Top-level directory prefix (e.g., "src", "docs").
    dir_prefix: String,
    /// The raw diff text for this file, including its trailing newline.
    diff: String,
    /// Byte size of the diff.
    size: usize,
}

impl FileDiff {
    fn new(path: String, diff: String) -> Self {
        Self {
            dir_prefix: get_dir_prefix(&path),
            size: diff.len(),
            path,
            diff,
        }
    }
}

/// Split raw diff text into per-file blocks without losing a byte.
///
/// Blocks start at `diff --git` lines (what git emits). A diff without any
/// such line (a hand-made unified diff) is split at `--- `/`+++ ` header
/// pairs instead; as a last resort the whole diff is one block.
fn split_by_file(diff: &str) -> Vec<FileDiff> {
    if diff.is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<usize> = Vec::new();
    let mut offset = 0;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    if starts.is_empty() {
        starts = plain_header_starts(diff);
    }
    if starts.is_empty() {
        debug!("no file headers in diff, treating it as one block");
        return vec![FileDiff::new("all".to_string(), diff.to_string())];
    }
    // Anything before the first header (e.g. a commit message from
    // `git show`) travels with the first file.
    starts[0] = 0;
    let mut blocks = Vec::with_capacity(starts.len());
    for (i, &begin) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(diff.len());
        let text = &diff[begin..end];
        blocks.push(FileDiff::new(path_of_block(text), text.to_string()));
    }
    blocks
}

/// Offsets of the `--- ` lines that begin a file in a diff without
/// `diff --git` lines. A header is a `--- ` line followed by a `+++ ` line and
/// then a `@@ ` hunk header; a removed line that happens to start with `-- `
/// is never followed by both.
fn plain_header_starts(diff: &str) -> Vec<usize> {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in diff.split_inclusive('\n') {
        lines.push((offset, line));
        offset += line.len();
    }
    let mut starts = Vec::new();
    for (i, (at, line)) in lines.iter().enumerate() {
        if line.starts_with("--- ")
            && lines.get(i + 1).is_some_and(|(_, l)| l.starts_with("+++ "))
            && lines.get(i + 2).is_some_and(|(_, l)| l.starts_with("@@ "))
        {
            starts.push(*at);
        }
    }
    starts
}

/// File path of a block: the `+++ b/` path, the `--- a/` path for a deleted
/// file, or the path from the `diff --git` line.
fn path_of_block(block: &str) -> String {
    let mut old: Option<String> = None;
    for line in block.lines() {
        if line.starts_with("@@ ") {
            break;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest.starts_with("/dev/null") {
                continue;
            }
            return clean_path(rest.strip_prefix("b/").unwrap_or(rest));
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            if !rest.starts_with("/dev/null") {
                old = Some(clean_path(rest.strip_prefix("a/").unwrap_or(rest)));
            }
        }
    }
    if let Some(p) = old {
        return p;
    }
    // Binary or mode-only change: take it from `diff --git a/x b/y`.
    block
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("diff --git "))
        .and_then(|l| l.rsplit_once(" b/").map(|(_, p)| clean_path(p)))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Strip quotes and a trailing tab-separated timestamp from a header path.
fn clean_path(p: &str) -> String {
    p.split('\t')
        .next()
        .unwrap_or(p)
        .trim()
        .trim_matches('"')
        .to_string()
}

/// Split a file block larger than `max_size` at hunk boundaries.
///
/// Every piece is the file header plus a run of whole hunks. A block with a
/// single hunk (or none) is returned as is.
fn split_oversize(fd: FileDiff, max_size: usize) -> Vec<FileDiff> {
    if fd.size <= max_size {
        return vec![fd];
    }
    let mut header_end: Option<usize> = None;
    let mut hunk_starts: Vec<usize> = Vec::new();
    let mut offset = 0;
    for line in fd.diff.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            header_end.get_or_insert(offset);
            hunk_starts.push(offset);
        }
        offset += line.len();
    }
    let Some(header_end) = header_end else {
        return vec![fd];
    };
    if hunk_starts.len() < 2 {
        return vec![fd];
    }
    let header = &fd.diff[..header_end];
    let mut pieces: Vec<FileDiff> = Vec::new();
    let mut current = String::from(header);
    for (i, &start) in hunk_starts.iter().enumerate() {
        let end = hunk_starts.get(i + 1).copied().unwrap_or(fd.diff.len());
        let hunk = &fd.diff[start..end];
        // `current` holds a hunk once it is longer than the bare header.
        if current.len() > header.len() && current.len() + hunk.len() > max_size {
            pieces.push(FileDiff::new(
                fd.path.clone(),
                std::mem::replace(&mut current, String::from(header)),
            ));
        }
        current.push_str(hunk);
    }
    pieces.push(FileDiff::new(fd.path.clone(), current));
    pieces
}

/// Group file diffs into chunks that fit within max_size.
fn group_by_size(file_diffs: &[FileDiff], max_size: usize) -> Vec<Vec<&FileDiff>> {
    let mut groups: Vec<Vec<&FileDiff>> = Vec::new();
    let mut current_group: Vec<&FileDiff> = Vec::new();
    let mut current_size: usize = 0;

    for fd in file_diffs {
        // If adding this file exceeds max_size, start a new group
        if !current_group.is_empty() && current_size + fd.size > max_size {
            groups.push(current_group);
            current_group = Vec::new();
            current_size = 0;
        }

        // Even if a single hunk exceeds max_size, it gets its own group
        current_size += fd.size;
        current_group.push(fd);
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    groups
}

/// Extract top-level directory prefix from a file path.
fn get_dir_prefix(path: &str) -> String {
    let path = path.trim_start_matches("a/").trim_start_matches("b/");
    path.split('/').next().unwrap_or("root").to_string()
}

/// Derive a human-readable label for a chunk group.
fn derive_label(group: &[&FileDiff]) -> String {
    if group.len() == 1 {
        let path = group[0].dir_prefix.clone();
        format!("{}/*", path)
    } else {
        let dirs: Vec<&str> = group.iter().map(|f| f.dir_prefix.as_str()).collect();
        let unique: Vec<&str> = {
            let mut d = dirs.clone();
            d.sort();
            d.dedup();
            d
        };
        if unique.len() == 1 {
            format!("{}/*", unique[0])
        } else if unique.len() <= 3 {
            unique.join(", ")
        } else {
            format!("{} dirs", unique.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::diff_parser;

    fn sample_diff() -> &'static str {
        "\
diff --git a/src/main.rs b/src/main.rs
index abc123..def456 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
+    // added line
 }

diff --git a/src/lib.rs b/src/lib.rs
new file mode 100644
--- /dev/null
+++ b/src/lib.rs
@@ -0,0 +1,3 @@
+pub fn add(a: i32, b: i32) -> i32 {
+    a + b
+}

diff --git a/docs/README.md b/docs/README.md
--- a/docs/README.md
+++ b/docs/README.md
@@ -1,3 +1,4 @@
 # Title
+Added line
 Some text
 End
"
    }

    /// Regression: every file of the input must reach some chunk, and each
    /// chunk must parse back into the files it claims (#720).
    #[test]
    fn every_file_survives_chunking() {
        for max in [100_000usize, 50] {
            let chunks = chunk_diff(sample_diff(), max);
            let mut seen: Vec<String> = Vec::new();
            for c in &chunks {
                for f in diff_parser::parse_diff(&c.diff) {
                    seen.push(f.new_path.or(f.old_path).unwrap_or_default());
                }
            }
            seen.sort();
            assert_eq!(
                seen,
                vec!["docs/README.md", "src/lib.rs", "src/main.rs"],
                "max={max}"
            );
        }
    }

    #[test]
    fn chunk_diff_single_chunk_when_small() {
        let chunks = chunk_diff(sample_diff(), 100_000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].total, 1);
    }

    #[test]
    fn chunk_diff_splits_when_exceeds_max() {
        // Very small max to force splitting
        let chunks = chunk_diff(sample_diff(), 50);
        assert!(chunks.len() > 1, "Should split into multiple chunks");
    }

    #[test]
    fn chunk_labels_are_human_readable() {
        let chunks = chunk_diff(sample_diff(), 50);
        for chunk in &chunks {
            assert!(!chunk.label.is_empty());
        }
    }

    #[test]
    fn empty_diff_returns_single_chunk() {
        let chunks = chunk_diff("", 1000);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn get_dir_prefix_strips_a_prefix() {
        assert_eq!(get_dir_prefix("a/src/main.rs"), "src");
        assert_eq!(get_dir_prefix("b/docs/README.md"), "docs");
        assert_eq!(get_dir_prefix("lib.rs"), "lib.rs");
    }

    #[test]
    fn group_by_size_respects_max() {
        let files = vec![
            FileDiff::new("src/a.rs".to_string(), "a".repeat(100)),
            FileDiff::new("docs/b.md".to_string(), "b".repeat(100)),
            FileDiff::new("src/c.rs".to_string(), "c".repeat(50)),
        ];
        let groups = group_by_size(&files, 150);
        // First two files = 200 > 150, so first file alone
        // Second file (100) + third (50) = 150 <= 150, together
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn derive_label_single_file() {
        let fd = FileDiff::new("src/a.rs".to_string(), String::new());
        let files = vec![&fd];
        assert_eq!(derive_label(&files), "src/*");
    }

    #[test]
    fn derive_label_multiple_dirs() {
        let fd1 = FileDiff::new("src/a.rs".to_string(), String::new());
        let fd2 = FileDiff::new("docs/b.md".to_string(), String::new());
        let files = vec![&fd1, &fd2];
        assert_eq!(derive_label(&files), "docs, src");
    }

    // ─── #720: lossless splitting, hunk split, exclusion ───

    fn matcher(pats: &[&str]) -> PathMatcher {
        PathMatcher::new(&pats.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    /// One file with `n` hunks of a few lines each.
    fn many_hunks(path: &str, n: usize) -> String {
        let mut d = format!(
            "diff --git a/{path} b/{path}\nindex 1..2 100644\n--- a/{path}\n+++ b/{path}\n"
        );
        for i in 0..n {
            let l = i * 100 + 1;
            d.push_str(&format!(
                "@@ -{l},2 +{l},3 @@ fn f{i}()\n ctx\n-old{i}\n+new{i}\n+extra{i}\n"
            ));
        }
        d
    }

    #[test]
    fn chunks_are_lossless_for_a_git_diff() {
        let diff = sample_diff();
        let joined: String = chunk_diff(diff, 100_000)
            .iter()
            .map(|c| c.diff.as_str())
            .collect();
        assert_eq!(joined, diff);
    }

    #[test]
    fn oversize_file_splits_at_hunks_and_stays_lossless() {
        let diff = many_hunks("src/big.rs", 40);
        let max = diff.len() / 4;
        let chunks = chunk_diff(&diff, max);
        assert!(chunks.len() >= 4, "got {}", chunks.len());
        let mut hunks = 0;
        for c in &chunks {
            // Every piece repeats the file header and parses as one file.
            let files = diff_parser::parse_diff(&c.diff);
            assert_eq!(files.len(), 1, "chunk: {}", c.diff);
            assert_eq!(files[0].new_path.as_deref(), Some("src/big.rs"));
            hunks += files[0].chunks.len();
            assert_eq!(c.files, vec!["src/big.rs".to_string()]);
        }
        assert_eq!(hunks, 40, "no hunk may be lost or duplicated");
    }

    #[test]
    fn a_single_oversize_hunk_still_gets_a_chunk() {
        let diff = many_hunks("src/one.rs", 1);
        let chunks = chunk_diff(&diff, 10);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].diff, diff);
    }

    #[test]
    fn plain_unified_diff_splits_per_file() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n+b\n--- a/y.rs\n+++ b/y.rs\n@@ -1 +1 @@\n-c\n+d\n";
        let blocks = split_by_file(diff);
        let paths: Vec<&str> = blocks.iter().map(|b| b.path.as_str()).collect();
        assert_eq!(paths, vec!["x.rs", "y.rs"]);
        let joined: String = blocks.iter().map(|b| b.diff.as_str()).collect();
        assert_eq!(joined, diff);
    }

    #[test]
    fn binary_and_deleted_files_keep_their_path() {
        let diff = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\ndiff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let paths: Vec<String> = split_by_file(diff).into_iter().map(|b| b.path).collect();
        assert_eq!(paths, vec!["img.png", "gone.rs"]);
    }

    #[test]
    fn exclude_paths_removes_matching_files_only() {
        let out = exclude_paths(sample_diff(), &matcher(&["docs/**", "*.lock"]));
        assert_eq!(out.excluded, vec!["docs/README.md"]);
        assert!(!out.diff.contains("README.md"));
        assert!(out.diff.contains("src/main.rs") && out.diff.contains("src/lib.rs"));
    }

    #[test]
    fn exclude_paths_with_no_patterns_is_identity() {
        let out = exclude_paths(sample_diff(), &PathMatcher::default());
        assert_eq!(out.diff, sample_diff());
        assert!(out.excluded.is_empty());
    }

    #[test]
    fn exclude_paths_can_remove_everything() {
        let out = exclude_paths(sample_diff(), &matcher(&["**"]));
        assert!(out.diff.is_empty());
        assert_eq!(out.excluded.len(), 3);
    }
}
