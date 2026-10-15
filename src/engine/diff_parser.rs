/// Unified diff parser — converts raw diff text into structured [`FileChunk`] data.
///
/// Simple regex-based parser that handles:
/// - Added, modified, deleted, and binary files
/// - Renames (old → new path)
/// - Multi-hunk diffs with line tracking
/// - Language detection from file extensions
use regex::Regex;
use std::sync::LazyLock;
use tracing::debug;

/// A single file's worth of changes in a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChunk {
    /// Old file path (from `--- a/path`). `None` for new files.
    pub old_path: Option<String>,
    /// New file path (from `+++ b/path`). `None` for deleted files.
    pub new_path: Option<String>,
    /// Language detected from file extension (e.g., `"rs"`, `"py"`).
    pub language: String,
    /// The `@@ ... @@` hunks inside this file.
    pub chunks: Vec<DiffHunk>,
    /// Whether this is a binary file change.
    pub is_binary: bool,
    /// Whether this file was deleted.
    pub is_deleted: bool,
    /// Whether this file is newly created.
    pub is_new: bool,
}

/// A single `@@ ... @@` hunk block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// The header text after the `@@` markers (function context).
    pub header: String,
    /// Individual diff lines within this hunk.
    pub lines: Vec<DiffLine>,
}

/// A single line inside a diff hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether this line was added, removed, or is context.
    pub line_type: DiffLineType,
    /// Content stripped of the leading `+`/`-`/` ` prefix.
    pub content: String,
    /// Line number in the old file (for `Remove` and `Context` lines).
    pub old_line_no: Option<u32>,
    /// Line number in the new file (for `Add` and `Context` lines).
    pub new_line_no: Option<u32>,
}

/// Type of a diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineType {
    Add,
    Remove,
    Context,
}

// ─── Regex patterns (compiled once) ───

static RE_FILE_HEADER_OLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---\s+(?:a/)?(.*)").unwrap());
static RE_FILE_HEADER_NEW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\+\+\+\s+(?:b/)?(.*)").unwrap());
static RE_HUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@\s-(\d+)(?:,(\d+))?\s\+(\d+)(?:,(\d+))?\s@@\s?(.*)").unwrap());
static RE_BINARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:Binary files|Binary file|GIT binary patch)").unwrap());

/// Parse a unified diff string into a list of [`FileChunk`] entries.
pub fn parse_diff(diff: &str) -> Vec<FileChunk> {
    let mut files = Vec::new();
    let mut current: Option<FileChunkBuilder> = None;

    for line in diff.lines() {
        // A `diff --git` line at column 0 is never hunk content (content lines
        // carry a `+`/`-`/` ` prefix), so it ends any open hunk even when the
        // header's line counts were wrong.
        if line.starts_with("diff --git ") {
            if let Some(builder) = current.as_mut() {
                builder.rem_old = 0;
                builder.rem_new = 0;
            }
            continue;
        }

        // Inside a hunk whose counts are not used up, `---` / `+++` are
        // CONTENT (a removed line `-- x`, an added line `++ y`), never file
        // headers. Treating them as headers lets a hostile diff re-file the
        // rest of a hunk under another path, or drop it entirely.
        let in_hunk = current
            .as_ref()
            .is_some_and(|b| b.rem_old > 0 || b.rem_new > 0);

        // Check for file header pair (--- / +++)
        if let Some(caps) = RE_FILE_HEADER_OLD.captures(line).filter(|_| !in_hunk) {
            // If we have a pending file, flush it
            if let Some(builder) = current.take() {
                files.push(builder.build());
            }
            let raw_path = caps[1].trim_end_matches('\t').to_string();
            let is_new = raw_path == "/dev/null";

            let mut builder = FileChunkBuilder::new();
            builder.old_path = if is_new { None } else { Some(raw_path) };
            builder.is_new = is_new;
            current = Some(builder);
            continue;
        }

        if let Some(ref mut builder) = current {
            if let Some(caps) = RE_FILE_HEADER_NEW.captures(line).filter(|_| !in_hunk) {
                let raw_path = caps[1].trim_end_matches('\t').to_string();
                let is_deleted = raw_path == "/dev/null";

                builder.new_path = if is_deleted { None } else { Some(raw_path) };
                builder.is_deleted = is_deleted;
                // Determine language from the new path (preferred) or old path
                let lang_path = builder.new_path.as_deref().or(builder.old_path.as_deref());
                if let Some(p) = lang_path {
                    builder.language = detect_language(p).to_string();
                }
                continue;
            }
        }

        // Inside a file section, check for hunks and lines
        if let Some(ref mut builder) = current {
            // Check for binary marker (before hunk check — binary files have no hunks)
            if RE_BINARY.is_match(line) {
                builder.is_binary = true;
                continue;
            }

            if let Some(caps) = RE_HUNK_HEADER.captures(line) {
                // Flush previous hunk (multi-hunk support)
                if let Some(hunk) = builder.current_hunk.take() {
                    builder.hunks.push(hunk);
                }

                let old_start: u32 = caps[1].parse().unwrap_or(1);
                let old_count: u32 = caps
                    .get(2)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(1);
                let new_start: u32 = caps[3].parse().unwrap_or(1);
                let new_count: u32 = caps
                    .get(4)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(1);
                let header = caps[5].to_string();

                // Initialize line counters from hunk header
                builder.old_line = old_start;
                builder.new_line = new_start;
                builder.rem_old = old_count;
                builder.rem_new = new_count;

                builder.current_hunk = Some(DiffHunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    header,
                    lines: Vec::new(),
                });
                continue;
            }

            if let Some(ref mut hunk) = builder.current_hunk {
                // Parse diff lines
                let (line_type, content) = if let Some(rest) = line.strip_prefix('+') {
                    (DiffLineType::Add, rest.to_string())
                } else if let Some(rest) = line.strip_prefix('-') {
                    (DiffLineType::Remove, rest.to_string())
                } else if let Some(rest) = line.strip_prefix(' ') {
                    (DiffLineType::Context, rest.to_string())
                } else {
                    // Unknown line (e.g., "diff --git ..."), skip
                    continue;
                };

                match line_type {
                    DiffLineType::Add => builder.rem_new = builder.rem_new.saturating_sub(1),
                    DiffLineType::Remove => builder.rem_old = builder.rem_old.saturating_sub(1),
                    DiffLineType::Context => {
                        builder.rem_old = builder.rem_old.saturating_sub(1);
                        builder.rem_new = builder.rem_new.saturating_sub(1);
                    }
                }

                // Compute line numbers based on line type, then increment counters
                let (old_line_no, new_line_no) = match line_type {
                    DiffLineType::Add => {
                        let no = if builder.new_line > 0 {
                            Some(builder.new_line)
                        } else {
                            None
                        };
                        builder.new_line += 1;
                        (None, no)
                    }
                    DiffLineType::Remove => {
                        let no = if builder.old_line > 0 {
                            Some(builder.old_line)
                        } else {
                            None
                        };
                        builder.old_line += 1;
                        (no, None)
                    }
                    DiffLineType::Context => {
                        let old_no = if builder.old_line > 0 {
                            Some(builder.old_line)
                        } else {
                            None
                        };
                        let new_no = if builder.new_line > 0 {
                            Some(builder.new_line)
                        } else {
                            None
                        };
                        builder.old_line += 1;
                        builder.new_line += 1;
                        (old_no, new_no)
                    }
                };

                hunk.lines.push(DiffLine {
                    line_type,
                    content,
                    old_line_no,
                    new_line_no,
                });
            }
        }
    }

    // Flush the last file
    if let Some(builder) = current.take() {
        files.push(builder.build());
    }

    debug!(file_count = files.len(), "parsed diff into file chunks");
    files
}

/// Detect programming language from a file path's extension.
///
/// Returns the language shorthand (e.g., `"rs"`, `"py"`, `"ts"`).
/// Falls back to `"unknown"` for unrecognized extensions.
pub fn detect_language(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some(ext) => match ext.to_lowercase().as_str() {
            "rs" => "rs",
            "py" | "pyi" => "py",
            "ts" | "tsx" => "ts",
            "js" | "jsx" | "mjs" | "cjs" => "js",
            "go" => "go",
            "java" => "java",
            "kt" | "kts" => "kt",
            "c" => "c",
            "cpp" | "cc" | "cxx" => "cpp",
            "h" | "hpp" | "hxx" => "h",
            "rb" => "rb",
            "php" => "php",
            "swift" => "swift",
            "scala" => "scala",
            "r" => "r",
            "sql" => "sql",
            "sh" | "bash" | "zsh" => "sh",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            "json" => "json",
            "md" | "markdown" => "md",
            "html" | "htm" => "html",
            "css" | "scss" | "sass" | "less" => "css",
            "dart" => "dart",
            "svelte" => "svelte",
            "lua" => "lua",
            "zig" => "zig",
            "ex" | "exs" => "ex",
            "proto" => "proto",
            "graphql" | "gql" => "graphql",
            "dockerfile" => "dockerfile",
            // Non-code extensions (still recognized for exclusion/filtering)
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico" | "webp" => "image",
            "pdf" | "doc" | "docx" => "document",
            "lock" | "sum" => "lock",
            _ => "unknown",
        },
        None => "unknown",
    }
}

/// Extract all added lines from parsed file chunks.
///
/// Returns `(file_path, line_number, content)` triples.
#[allow(dead_code)] // used by bundling in v0.5
pub fn extract_added_lines(chunks: &[FileChunk]) -> Vec<(String, u32, String)> {
    let mut result = Vec::new();

    for file in chunks {
        let path = file
            .new_path
            .as_deref()
            .or(file.old_path.as_deref())
            .unwrap_or("unknown");
        for hunk in &file.chunks {
            for line in &hunk.lines {
                if line.line_type == DiffLineType::Add {
                    if let Some(ln) = line.new_line_no {
                        result.push((path.to_string(), ln, line.content.clone()));
                    }
                }
            }
        }
    }

    result
}

// ─── Internal builder for assembling FileChunk ───

struct FileChunkBuilder {
    old_path: Option<String>,
    new_path: Option<String>,
    language: String,
    current_hunk: Option<DiffHunk>,
    hunks: Vec<DiffHunk>,
    old_line: u32,
    new_line: u32,
    /// Old / new lines still expected in the open hunk (from its `@@` counts).
    rem_old: u32,
    rem_new: u32,
    is_binary: bool,
    is_deleted: bool,
    is_new: bool,
}

impl FileChunkBuilder {
    fn new() -> Self {
        Self {
            old_path: None,
            new_path: None,
            language: "unknown".to_string(),
            current_hunk: None,
            hunks: Vec::new(),
            old_line: 0,
            new_line: 0,
            rem_old: 0,
            rem_new: 0,
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }
    }

    fn build(mut self) -> FileChunk {
        if let Some(hunk) = self.current_hunk.take() {
            self.hunks.push(hunk);
        }

        // Validate hunk line counts — warn if actual lines don't match header counts
        for hunk in &self.hunks {
            let actual_old = hunk
                .lines
                .iter()
                .filter(|l| {
                    l.line_type == DiffLineType::Remove || l.line_type == DiffLineType::Context
                })
                .count() as u32;
            let actual_new = hunk
                .lines
                .iter()
                .filter(|l| {
                    l.line_type == DiffLineType::Add || l.line_type == DiffLineType::Context
                })
                .count() as u32;

            if actual_old > 0 && actual_old != hunk.old_count {
                debug!(
                    old_expected = hunk.old_count,
                    old_actual = actual_old,
                    new_path = ?self.new_path,
                    "hunk line count mismatch (old) — diff may be truncated"
                );
            }
            if actual_new > 0 && actual_new != hunk.new_count {
                debug!(
                    new_expected = hunk.new_count,
                    new_actual = actual_new,
                    new_path = ?self.new_path,
                    "hunk line count mismatch (new) — diff may be truncated"
                );
            }
        }

        FileChunk {
            old_path: self.old_path,
            new_path: self.new_path,
            language: self.language,
            chunks: self.hunks,
            is_binary: self.is_binary,
            is_deleted: self.is_deleted,
            is_new: self.is_new,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_diff() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 use std::io;
 fn main() {
+    println!("hello");
 }
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.old_path.as_deref(), Some("src/main.rs"));
        assert_eq!(f.new_path.as_deref(), Some("src/main.rs"));
        assert_eq!(f.language, "rs");
        assert_eq!(f.chunks.len(), 1);
        assert_eq!(f.chunks[0].lines.len(), 4);

        let added: Vec<_> = f.chunks[0]
            .lines
            .iter()
            .filter(|l| l.line_type == DiffLineType::Add)
            .collect();
        assert_eq!(added.len(), 1);
        assert!(added[0].content.contains("println"));
    }

    #[test]
    fn parse_new_file() {
        let diff = r#"diff --git a/src/new_file.py b/src/new_file.py
new file mode 100644
--- /dev/null
+++ b/src/new_file.py
@@ -0,0 +1,5 @@
+import os
+
+def main():
+    pass
+
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.is_new);
        assert!(!f.is_deleted);
        assert!(f.old_path.is_none());
        assert_eq!(f.new_path.as_deref(), Some("src/new_file.py"));
        assert_eq!(f.language, "py");
    }

    #[test]
    fn parse_deleted_file() {
        let diff = r#"diff --git a/old_file.go b/old_file.go
deleted file mode 100644
--- a/old_file.go
+++ /dev/null
@@ -1,3 +0,0 @@
-package main
-
-func deleted() {}
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.is_deleted);
        assert!(!f.is_new);
        assert!(f.new_path.is_none());
        assert_eq!(f.old_path.as_deref(), Some("old_file.go"));
        assert_eq!(f.language, "go");
    }

    #[test]
    fn parse_binary_file() {
        let diff = r#"diff --git a/image.png b/image.png
--- a/image.png
+++ b/image.png
Binary files a/image.png and b/image.png differ
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.is_binary);
        assert_eq!(f.language, "image");
        assert!(f.chunks.is_empty());
    }

    #[test]
    fn parse_multifile_diff() {
        let diff = r#"diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -1,1 +1,2 @@
 pub mod foo;
+pub mod bar;
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,3 +1,3 @@
 fn foo() {
-    old();
+    new();
 }
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].new_path.as_deref(), Some("lib.rs"));
        assert_eq!(files[1].new_path.as_deref(), Some("src/foo.rs"));
    }

    #[test]
    fn parse_multi_hunk_diff() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn a() {
-    old_a();
+    new_a();
 }
@@ -10,3 +10,3 @@
 fn b() {
-    old_b();
+    new_b();
 }
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].chunks.len(), 2);
        assert_eq!(files[0].chunks[0].old_start, 1);
        assert_eq!(files[0].chunks[1].old_start, 10);
    }

    #[test]
    fn line_numbers_tracked() {
        let diff = r#"diff --git a/test.ts b/test.ts
--- a/test.ts
+++ b/test.ts
@@ -5,4 +5,5 @@
 const x = 1;
-const y = 2;
+const y = 3;
+const z = 4;
 const w = 5;
"#;
        let files = parse_diff(diff);
        let hunk = &files[0].chunks[0];
        // Context line at old=5, new=5
        let ctx = &hunk.lines[0];
        assert_eq!(ctx.line_type, DiffLineType::Context);
        assert_eq!(ctx.old_line_no, Some(5));
        assert_eq!(ctx.new_line_no, Some(5));
        // Removed line at old=6
        let rem = &hunk.lines[1];
        assert_eq!(rem.line_type, DiffLineType::Remove);
        assert_eq!(rem.old_line_no, Some(6));
        assert!(rem.new_line_no.is_none());
        // First added line at new=6
        let add1 = &hunk.lines[2];
        assert_eq!(add1.line_type, DiffLineType::Add);
        assert!(add1.old_line_no.is_none());
        assert_eq!(add1.new_line_no, Some(6));
        // Second added line at new=7
        let add2 = &hunk.lines[3];
        assert_eq!(add2.new_line_no, Some(7));
    }

    #[test]
    fn extract_added_lines_works() {
        let diff = r#"diff --git a/app.js b/app.js
--- a/app.js
+++ b/app.js
@@ -1,1 +1,3 @@
 console.log('a');
+console.log('b');
+console.log('c');
"#;
        let files = parse_diff(diff);
        let added = extract_added_lines(&files);
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].0, "app.js");
        assert_eq!(added[0].1, 2);
        assert_eq!(added[1].1, 3);
    }

    #[test]
    fn detect_language_various() {
        assert_eq!(detect_language("src/main.rs"), "rs");
        assert_eq!(detect_language("script.py"), "py");
        assert_eq!(detect_language("index.ts"), "ts");
        assert_eq!(detect_language("app.tsx"), "ts");
        assert_eq!(detect_language("server.js"), "js");
        assert_eq!(detect_language("main.go"), "go");
        assert_eq!(detect_language("App.java"), "java");
        assert_eq!(detect_language("Makefile"), "unknown");
        assert_eq!(detect_language("path/to/no_ext"), "unknown");
        assert_eq!(detect_language("style.scss"), "css");
    }

    #[test]
    fn empty_diff_returns_empty() {
        let files = parse_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn hunk_header_with_function_context() {
        let diff = r#"diff --git a/lib.rs b/lib.rs
--- a/lib.rs
+++ b/lib.rs
@@ -10,7 +10,8 @@ pub fn process(input: &str) {
     let data = parse(input);
-    consume(data);
+    let result = transform(data);
+    consume(result);
 }
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let hunk = &files[0].chunks[0];
        assert_eq!(hunk.old_start, 10);
        assert_eq!(hunk.old_count, 7);
        assert_eq!(hunk.new_start, 10);
        assert_eq!(hunk.new_count, 8);
        assert!(hunk.header.contains("pub fn process"));
    }

    #[test]
    fn hunk_header_without_line_count() {
        // Regression test: bare @@ -1 +1 @@ without ,count must not panic
        let diff = r#"diff --git a/main.rs b/main.rs
--- a/main.rs
+++ b/main.rs
@@ -1 +1 @@
-use foo::bar;
+use baz::qux;
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        let hunk = &files[0].chunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.old_count, 1);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 1);
    }

    #[test]
    fn truncated_diff_returns_partial_parse() {
        // Simulate truncated diff — hunk declares 3 lines but only 1 present
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
+    println!("hello");
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        // Should still parse the lines that are present
        assert_eq!(files[0].chunks[0].lines.len(), 2);
    }

    #[test]
    fn git_binary_patch_detected() {
        let diff = r#"diff --git a/image.png b/image.png
--- a/image.png
+++ b/image.png
GIT binary patch
literal 1234
zcmexABCDE
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_binary);
    }

    #[test]
    fn binary_file_marker_variants() {
        // Test "Binary file" singular form
        let diff = r#"diff --git a/logo.png b/logo.png
--- a/logo.png
+++ b/logo.png
Binary file a/logo.png differs from b/logo.png
"#;
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_binary);
    }

    // ─── hostile hunk content must not be read as file headers ───

    #[test]
    fn removed_line_that_looks_like_old_header_does_not_drop_the_hunk() {
        // `-- old note` removed => the diff line is `--- old note`.
        let diff = concat!(
            "diff --git a/q.sql b/q.sql\n",
            "--- a/q.sql\n",
            "+++ b/q.sql\n",
            "@@ -1,1 +1,1 @@\n",
            "--- old note\n",
            "+password = \"hunter2hunter2\"\n",
        );
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1, "no phantom file entry");
        let lines = &files[0].chunks[0].lines;
        assert!(
            lines
                .iter()
                .any(|l| l.line_type == DiffLineType::Add && l.content.contains("hunter2")),
            "the added line must still be parsed"
        );
    }

    #[test]
    fn added_line_that_looks_like_new_header_does_not_refile_the_hunk() {
        // `++ b/tests/fixture.py` added => the diff line is `+++ b/tests/fixture.py`.
        let diff = concat!(
            "diff --git a/app.py b/app.py\n",
            "--- a/app.py\n",
            "+++ b/app.py\n",
            "@@ -1,1 +1,3 @@\n",
            " x = 1\n",
            "+++ b/tests/fixture.py\n",
            "+password = \"hunter2hunter2\"\n",
        );
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].new_path.as_deref(), Some("app.py"));
        assert_eq!(files[0].chunks[0].lines.len(), 3);
    }

    #[test]
    fn next_file_is_still_found_after_a_hunk_with_correct_counts() {
        let diff = concat!(
            "--- a/one.rs\n",
            "+++ b/one.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-a\n",
            "+b\n",
            "--- a/two.rs\n",
            "+++ b/two.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-c\n",
            "+d\n",
        );
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].new_path.as_deref(), Some("two.rs"));
    }

    #[test]
    fn git_diff_line_ends_a_hunk_even_when_counts_are_wrong() {
        // Header claims 9 old / 9 new lines but the hunk has 1: the next
        // `diff --git` must still start a new file.
        let diff = concat!(
            "diff --git a/one.rs b/one.rs\n",
            "--- a/one.rs\n",
            "+++ b/one.rs\n",
            "@@ -1,9 +1,9 @@\n",
            "-a\n",
            "+b\n",
            "diff --git a/two.rs b/two.rs\n",
            "--- a/two.rs\n",
            "+++ b/two.rs\n",
            "@@ -1,1 +1,1 @@\n",
            "-c\n",
            "+d\n",
        );
        let files = parse_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].new_path.as_deref(), Some("two.rs"));
    }
}
