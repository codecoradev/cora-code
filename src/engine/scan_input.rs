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

use std::path::Path;

use crate::engine::diff_parser::{DiffLineType, FileChunk};
use crate::engine::scanner::FileEntry;

/// One file's worth of lines to scan.
#[derive(Debug, Clone)]
pub struct ScanFile<'a> {
    /// Path used in findings and for path-aware classification.
    pub path: &'a str,
    /// Language tag (file extension for files, the chunk language for diffs).
    pub language: &'a str,
    /// `(line number, text)`. Diff lines without a number carry `0`.
    pub lines: Vec<(u32, &'a str)>,
}

impl<'a> ScanFile<'a> {
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
        Self {
            path: chunk
                .new_path
                .as_deref()
                .or(chunk.old_path.as_deref())
                .unwrap_or("unknown"),
            language: chunk.language.as_str(),
            lines,
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
        Self {
            path: chunk
                .new_path
                .as_deref()
                .or(chunk.old_path.as_deref())
                .unwrap_or(""),
            language: chunk.language.as_str(),
            lines,
        }
    }

    /// Every line of a whole file, numbered from 1. The language is the file
    /// extension (`""` when there is none).
    pub fn from_entry(entry: &'a FileEntry) -> Self {
        Self {
            path: entry.path.as_str(),
            language: Path::new(&entry.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(""),
            lines: entry
                .content
                .lines()
                .zip(1u32..)
                .map(|(t, n)| (n, t))
                .collect(),
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
}
