//! Scanner input seam (#610): "the lines of a file", independent of how they
//! were obtained.
//!
//! The deterministic scanners (`secrets_scanner`, `security_scanner`,
//! `rules::run_rules`) and the inline `cora-ignore:` filter only need a path, a
//! language and `(line number, text)` pairs. [`ScanFile`] is that shape; the
//! diff and whole-file sources are thin adapters over it:
//!
//! - [`ScanFile::added_lines`]: the added lines of a diff chunk (what the
//!   scanners looked at before; `cora review`).
//! - [`ScanFile::post_image`]: added + context lines of a diff chunk (what the
//!   inline-suppression filter reads).
//! - [`ScanFile::from_entry`]: every line of a scanned file (`cora scan`, MCP
//!   snippet check).

use std::collections::HashSet;
use std::path::Path;

use crate::engine::diff_parser::{DiffLineType, FileChunk};
use crate::engine::scanner::FileEntry;
use crate::engine::test_context;

/// One file's worth of lines to scan.
#[derive(Debug, Clone)]
pub struct ScanFile<'a> {
    /// Path used in findings and for path-aware classification.
    pub path: &'a str,
    /// Language tag (file extension for files, the chunk language for diffs).
    pub language: &'a str,
    /// `(line number, text)`. Diff lines without a number carry `0`.
    pub lines: Vec<(u32, &'a str)>,
    /// The whole file is test code (by path: `tests/`, `*_test.*`, ...).
    pub test_path: bool,
    /// Lines inside inline test code (Rust `#[cfg(test)]` modules, `#[test]`
    /// functions), see [`test_context`].
    pub test_lines: HashSet<u32>,
}

impl<'a> ScanFile<'a> {
    /// Is `line` test code (whole-file test path or an inline test block)?
    pub fn in_test(&self, line: u32) -> bool {
        self.test_path || self.test_lines.contains(&line)
    }

    /// This file with test awareness switched off
    /// (`rules_engine.include_tests: true`).
    pub fn ignoring_tests(mut self) -> Self {
        self.test_path = false;
        self.test_lines.clear();
        self
    }

    /// Inline test lines of a diff chunk: Rust test blocks visible in its
    /// post-image, plus every line of a hunk that sits inside `mod tests`.
    fn chunk_test_lines(chunk: &FileChunk) -> HashSet<u32> {
        let mut set = HashSet::new();
        let is_rust = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .is_some_and(|p| p.ends_with(".rs"));
        if !is_rust {
            return set;
        }
        let post: Vec<(u32, &str)> = chunk
            .chunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.line_type != DiffLineType::Remove)
            .filter_map(|l| l.new_line_no.map(|n| (n, l.content.as_str())))
            .collect();
        set.extend(test_context::rust_test_lines(&post, true));
        for hunk in &chunk.chunks {
            if test_context::hunk_header_is_test_module(&hunk.header) {
                set.extend(hunk.lines.iter().filter_map(|l| l.new_line_no));
            }
        }
        set
    }

    /// Added lines of a diff chunk, in diff order. A missing path becomes
    /// `"unknown"` and a missing new-side line number becomes `0`, exactly as
    /// the scanners treated them before this seam existed.
    pub fn added_lines(chunk: &'a FileChunk) -> Self {
        let lines = chunk
            .chunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.line_type == DiffLineType::Add)
            .map(|l| (l.new_line_no.unwrap_or(0), l.content.as_str()))
            .collect();
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("unknown");
        Self {
            path,
            language: chunk.language.as_str(),
            lines,
            test_path: test_context::is_test_path(path),
            test_lines: Self::chunk_test_lines(chunk),
        }
    }

    /// Post-change lines (added + context, with a new-side number) of a diff
    /// chunk. A missing path becomes `""`.
    pub fn post_image(chunk: &'a FileChunk) -> Self {
        let lines = chunk
            .chunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.line_type != DiffLineType::Remove)
            .filter_map(|l| l.new_line_no.map(|n| (n, l.content.as_str())))
            .collect();
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        Self {
            path,
            language: chunk.language.as_str(),
            lines,
            test_path: test_context::is_test_path(path),
            test_lines: Self::chunk_test_lines(chunk),
        }
    }

    /// Every line of a whole file, numbered from 1. The language is the file
    /// extension (`""` when there is none).
    pub fn from_entry(entry: &'a FileEntry) -> Self {
        let language = Path::new(&entry.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lines: Vec<(u32, &str)> = entry
            .content
            .lines()
            .zip(1u32..)
            .map(|(t, n)| (n, t))
            .collect();
        let test_lines = if language == "rs" {
            test_context::rust_test_lines(&lines, false)
        } else {
            HashSet::new()
        };
        Self {
            path: entry.path.as_str(),
            language,
            lines,
            test_path: test_context::is_test_path(&entry.path),
            test_lines,
        }
    }
}

/// Added-line view of every chunk (diff adapter).
pub fn from_chunks(chunks: &[FileChunk]) -> Vec<ScanFile<'_>> {
    chunks.iter().map(ScanFile::added_lines).collect()
}

/// Whole-file view of every entry (file adapter).
pub fn from_entries(entries: &[FileEntry]) -> Vec<ScanFile<'_>> {
    entries.iter().map(ScanFile::from_entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::diff_parser::parse_diff;

    #[test]
    fn file_entry_is_numbered_from_one_with_extension_language() {
        let e = FileEntry {
            path: "src/a.py".into(),
            content: "x = 1\ny = 2\n".into(),
            lines: 2,
        };
        let f = ScanFile::from_entry(&e);
        assert_eq!(f.path, "src/a.py");
        assert_eq!(f.language, "py");
        assert_eq!(f.lines, vec![(1, "x = 1"), (2, "y = 2")]);
    }

    #[test]
    fn diff_adapters_split_added_from_post_image() {
        let diff = "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1,2 +1,3 @@\n ctx\n-gone\n+added\n tail\n";
        let chunks = parse_diff(diff);
        let added = ScanFile::added_lines(&chunks[0]);
        assert_eq!(added.lines, vec![(2, "added")]);
        let post = ScanFile::post_image(&chunks[0]);
        assert_eq!(post.lines, vec![(1, "ctx"), (2, "added"), (3, "tail")]);
    }

    #[test]
    fn scan_file_knows_inline_test_lines_and_test_paths() {
        let e = FileEntry {
            path: "src/a.rs".into(),
            content: "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n".into(),
            lines: 5,
        };
        let f = ScanFile::from_entry(&e);
        assert!(!f.in_test(1));
        assert!((2..=5).all(|n| f.in_test(n)));
        assert!(!f.clone().ignoring_tests().in_test(3));

        let t = FileEntry {
            path: "tests/it.rs".into(),
            content: "fn a() {}\n".into(),
            lines: 1,
        };
        assert!(
            ScanFile::from_entry(&t).in_test(1),
            "whole file is test code"
        );
    }

    #[test]
    fn diff_hunk_inside_mod_tests_is_test_code() {
        // git prints the enclosing `mod tests {` as the hunk header.
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -40,2 +40,3 @@ mod tests {\n     fn t() {\n+        x.unwrap();\n     }\n";
        let chunks = parse_diff(diff);
        let added = ScanFile::added_lines(&chunks[0]);
        assert_eq!(added.lines, vec![(41, "        x.unwrap();")]);
        assert!(added.in_test(41));
    }
}
