/// Index-powered deterministic scanners — rules that require symbol graph intelligence.
///
/// Unlike regex-based scanners (secrets, security), these scanners query the
/// symbol index to detect issues impossible with pattern matching alone:
/// - Unused imports (needs cross-reference between imports and usages)
/// - Dead code in changed files (needs caller graph)
/// - Breaking changes (needs cross-file caller resolution)
use tracing::debug;

use crate::engine::Severity;
use crate::engine::diff_parser::{DiffLineType, FileChunk};
use crate::engine::rules::types::RuleFinding;
use crate::index::graph;

/// Check if a file path matches any of the skip patterns.
/// Uses simple glob matching (* and **) against both the full path and the basename.
/// Pattern "src/main.ts" matches exactly, "*.config.ts" matches any filename ending in .config.ts.
pub fn should_skip_file(file_path: &str, skip_patterns: &[String]) -> bool {
    if skip_patterns.is_empty() {
        return false;
    }

    let basename = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    for pattern in skip_patterns {
        // Exact match (e.g. "src/main.ts")
        if file_path == pattern {
            return true;
        }
        // Basename exact match
        if basename == *pattern {
            return true;
        }
        // Simple glob: pattern contains *
        if pattern.contains('*') {
            // Convert simple glob to a basic match:
            // "*.config.ts" → check if basename ends with ".config.ts"
            // "vite.config.*" → check if basename starts with "vite.config."
            if pattern.starts_with("*.") {
                let suffix = &pattern[1..]; // ".config.ts"
                if basename.ends_with(suffix) {
                    return true;
                }
            } else if pattern.ends_with(".*") {
                let prefix = &pattern[..pattern.len() - 2]; // "vite.config"
                if basename.starts_with(prefix) {
                    return true;
                }
            }
            // "**/something" → match basename
            if let Some(rest) = pattern.strip_prefix("**/") {
                if basename == rest || file_path.ends_with(&format!("/{}", rest)) {
                    return true;
                }
            }
        }
    }

    false
}

/// Scan for unused imports across all changed files using the symbol index.
///
/// For each file with IMPORTS edges, checks if each imported symbol is actually
/// referenced in the file. Works only when a symbol index is available.
///
/// Returns `Vec<RuleFinding>` with severity `Minor` for each unused import.
pub fn scan_unused_imports(
    chunks: &[FileChunk],
    project_root: &std::path::Path,
    max_findings: usize,
    skip_patterns: &[String],
) -> Vec<RuleFinding> {
    let conn = match crate::index::open_global_index() {
        Ok(c) => c,
        Err(_) => {
            debug!("no global index available — skipping unused import scan");
            return Vec::new();
        }
    };

    let project_id = match crate::index::ensure_project(&conn, project_root) {
        Ok(id) => id,
        Err(_) => {
            debug!("failed to get project_id — skipping unused import scan");
            return Vec::new();
        }
    };

    let mut findings = Vec::new();

    // Collect unique changed files
    let mut seen_files = std::collections::HashSet::new();
    for chunk in chunks {
        let file = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("unknown");

        // Skip deleted files (no new_path) and unknown
        if chunk.new_path.is_none() {
            continue;
        }

        // Skip files matching skip patterns
        if should_skip_file(file, skip_patterns) {
            continue;
        }

        // Only check files with actual additions
        let has_additions = chunk
            .chunks
            .iter()
            .any(|h| h.lines.iter().any(|l| l.line_type == DiffLineType::Add));
        if !has_additions {
            continue;
        }

        if seen_files.insert(file.to_string()) {
            match graph::find_unused_imports(&conn, file, project_id) {
                Ok(unused) => {
                    for u in &unused {
                        findings.push(RuleFinding {
                            rule_id: "index-unused-import".to_string(),
                            file: u.file.clone(),
                            line: u.line,
                            severity: Severity::Minor,
                            title: format!("[index-unused-import] Unused import: {}", u.target),
                            body: format!(
                                "Import `{}` is never used in this file. \
                                 Consider removing it to keep imports clean.",
                                u.target
                            ),
                        });
                    }
                }
                Err(e) => {
                    debug!("unused import scan failed for {}: {}", file, e);
                }
            }
        }

        if findings.len() >= max_findings {
            break;
        }
    }

    // Cap findings
    findings.truncate(max_findings);

    debug!(count = findings.len(), "unused import scan complete");
    findings
}

/// Scan for dead code (unreachable symbols) in changed files using the symbol index.
///
/// For each changed file, finds functions/methods with zero callers in the
/// project. Excludes well-known names (main, new, drop, etc.) and test functions.
///
/// Returns `Vec<RuleFinding>` with severity `Info` for each dead symbol.
pub fn scan_dead_code_in_review(
    chunks: &[FileChunk],
    project_root: &std::path::Path,
    max_findings: usize,
    skip_patterns: &[String],
) -> Vec<RuleFinding> {
    let conn = match crate::index::open_global_index() {
        Ok(c) => c,
        Err(_) => {
            debug!("no global index available — skipping dead code scan");
            return Vec::new();
        }
    };

    let project_id = match crate::index::ensure_project(&conn, project_root) {
        Ok(id) => id,
        Err(_) => {
            debug!("failed to get project_id — skipping dead code scan");
            return Vec::new();
        }
    };

    let mut findings = Vec::new();

    let mut seen_files = std::collections::HashSet::new();
    for chunk in chunks {
        let file = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("unknown");

        if chunk.new_path.is_none() {
            continue;
        }

        if should_skip_file(file, skip_patterns) {
            continue;
        }

        if seen_files.insert(file.to_string()) {
            match graph::find_dead_code_in_file(&conn, file, project_id, false) {
                Ok(dead) => {
                    for d in &dead {
                        findings.push(RuleFinding {
                            rule_id: "index-dead-code".to_string(),
                            file: d.file.clone(),
                            line: d.line,
                            severity: Severity::Info,
                            title: format!("[index-dead-code] Potentially dead code: {}", d.name),
                            body: format!(
                                "Function `{}` ({}) has no callers in the \
                                 project. Verify it's not called via reflection, \
                                 trait dispatch, or external entry points.",
                                d.name, d.kind
                            ),
                        });
                    }
                }
                Err(e) => {
                    debug!("dead code scan failed for {}: {}", file, e);
                }
            }
        }

        if findings.len() >= max_findings {
            break;
        }
    }

    findings.truncate(max_findings);

    debug!(count = findings.len(), "dead code scan complete");
    findings
}

/// Scan for potential breaking changes — removed or modified public symbols
/// that have existing callers in the project.
///
/// Analyzes the diff for removed lines containing public symbol definitions,
/// then cross-references the index to find callers.
///
/// Returns `Vec<RuleFinding>` with severity `Major` for each breaking change.
pub fn scan_breaking_changes(
    chunks: &[FileChunk],
    project_root: &std::path::Path,
    max_findings: usize,
    skip_patterns: &[String],
) -> Vec<RuleFinding> {
    let conn = match crate::index::open_global_index() {
        Ok(c) => c,
        Err(_) => {
            debug!("no global index available — skipping breaking change scan");
            return Vec::new();
        }
    };

    let project_id = match crate::index::ensure_project(&conn, project_root) {
        Ok(id) => id,
        Err(_) => {
            debug!("failed to get project_id — skipping breaking change scan");
            return Vec::new();
        }
    };

    let mut findings = Vec::new();

    // Patterns for public symbol removal across languages.
    // These are heuristic — not all removed lines match, but high-signal ones do.
    let removal_patterns: &[&str] = &[
        // Rust: pub fn/struct/enum/mod
        r"(?m)^(?:pub\s+)?(?:fn|struct|enum|trait|mod|type|const|static)\s+(\w+)",
        // TypeScript/JS: export function/const/class
        r"(?m)^export\s+(?:async\s+)?(?:function|const|class|interface|type)\s+(\w+)",
        // Go: func and type declarations
        r"(?m)^(?:func|type|var|const)\s+(\w+)",
        // Python: def and class
        r"(?m)^(?:async\s+)?(?:def|class)\s+(\w+)",
    ];

    let compiled: Vec<std::sync::Arc<regex::Regex>> = removal_patterns
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .map(std::sync::Arc::new)
        .collect();

    for chunk in chunks {
        let file = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("unknown");

        if should_skip_file(file, skip_patterns) {
            continue;
        }

        for hunk in &chunk.chunks {
            for line in &hunk.lines {
                // Only look at removed lines (old code being deleted)
                if line.line_type != DiffLineType::Remove {
                    continue;
                }

                // Try to match a public symbol definition being removed
                for re in &compiled {
                    if let Some(caps) = re.captures(&line.content) {
                        let symbol_name = &caps[1];

                        // Skip trivially short names
                        if symbol_name.len() < 3 {
                            continue;
                        }

                        let line_no = line.old_line_no.unwrap_or(0);

                        // Check if this symbol has callers in the index
                        match graph::find_callers(&conn, project_id, symbol_name, 10) {
                            Ok(callers) if !callers.is_empty() => {
                                let caller_list = callers
                                    .iter()
                                    .take(3)
                                    .map(|c| format!("{} ({}:{})", c.caller, c.file, c.line))
                                    .collect::<Vec<_>>()
                                    .join(", ");

                                findings.push(RuleFinding {
                                    rule_id: "index-breaking-change".to_string(),
                                    file: file.to_string(),
                                    line: line_no,
                                    severity: Severity::Major,
                                    title: format!(
                                        "[index-breaking-change] Removing `{}` \
                                         breaks {} caller(s)",
                                        symbol_name,
                                        callers.len()
                                    ),
                                    body: format!(
                                        "Symbol `{}` is being removed but has {} \
                                         caller(s): {}. This is a breaking change.",
                                        symbol_name,
                                        callers.len(),
                                        caller_list
                                    ),
                                });
                            }
                            _ => continue,
                        }
                    }
                }
            }
        }

        if findings.len() >= max_findings {
            break;
        }
    }

    findings.truncate(max_findings);

    debug!(count = findings.len(), "breaking change scan complete");
    findings
}

/// Scan a full project for index-based findings (unused imports + dead code).
/// Designed for `cora scan` which operates on file paths, not diffs.
/// Returns findings for any file in the project that has an index DB.
pub fn scan_project_index(
    root: &std::path::Path,
    files: &[crate::engine::scanner::FileEntry],
    max_findings: usize,
    skip_patterns: &[String],
) -> Vec<crate::engine::ReviewIssue> {
    use crate::engine::ReviewIssue;

    let mut findings = Vec::new();

    let conn = match crate::index::open_global_index() {
        Ok(c) => c,
        Err(_) => {
            debug!("no global index available — skipping project index scan");
            return findings;
        }
    };

    let project_id = match crate::index::ensure_project(&conn, root) {
        Ok(id) => id,
        Err(_) => {
            debug!("failed to get project_id — skipping project index scan");
            return findings;
        }
    };

    // Scan for unused imports across all files in the scan set
    let mut seen_files = std::collections::HashSet::new();
    for entry in files {
        if should_skip_file(&entry.path, skip_patterns) {
            continue;
        }
        if seen_files.insert(entry.path.clone()) {
            match graph::find_unused_imports(&conn, &entry.path, project_id) {
                Ok(unused) => {
                    for u in &unused {
                        findings.push(ReviewIssue {
                            file: u.file.clone(),
                            line: Some(u.line),
                            severity: crate::engine::Severity::Minor,
                            issue_type: Some("index".into()),
                            title: format!("[index-unused-import] Unused import: {}", u.target),
                            body: format!(
                                "Import `{}` is never used in this file. \
                                 Consider removing it to keep imports clean.",
                                u.target
                            ),
                            suggested_fix: None,
                        });
                    }
                }
                Err(e) => {
                    debug!("unused import scan failed for {}: {}", entry.path, e);
                }
            }
        }
        if findings.len() >= max_findings {
            break;
        }
    }

    if findings.len() >= max_findings {
        return findings;
    }

    // Scan for dead code in the indexed project
    let opts = graph::DeadCodeOptions::default();
    match graph::find_dead_code(&conn, project_id, &opts) {
        Ok(dead) => {
            for func in dead.into_iter().take(max_findings - findings.len()) {
                findings.push(ReviewIssue {
                    file: func.file.clone(),
                    line: Some(func.line),
                    severity: crate::engine::Severity::Info,
                    issue_type: Some("index".into()),
                    title: format!("[index-dead-code] Potentially dead code: {}", func.name),
                    body: format!(
                        "Function `{}` ({}) has no callers in the \
                         project. Verify it's not called via reflection, \
                         trait dispatch, or external entry points.",
                        func.name, func.kind
                    ),
                    suggested_fix: None,
                });
            }
        }
        Err(e) => {
            debug!("dead code scan failed: {}", e);
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a minimal FileChunk for testing.
    fn make_chunk(new_path: &str, content: &str) -> FileChunk {
        FileChunk {
            old_path: Some(new_path.to_string()),
            new_path: Some(new_path.to_string()),
            language: "rust".to_string(),
            chunks: vec![crate::engine::diff_parser::DiffHunk {
                old_start: 1,
                old_count: 0,
                new_start: 1,
                new_count: 0,
                header: "@@ -1 +1 @@".to_string(),
                lines: content
                    .lines()
                    .map(|l| crate::engine::diff_parser::DiffLine {
                        content: l.to_string(),
                        line_type: if l.starts_with('+') {
                            DiffLineType::Add
                        } else if l.starts_with('-') {
                            DiffLineType::Remove
                        } else {
                            DiffLineType::Context
                        },
                        old_line_no: None,
                        new_line_no: None,
                    })
                    .collect(),
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }
    }

    #[test]
    fn scan_unused_imports_no_index_graceful() {
        // No index available — should return empty, not panic
        let chunks = vec![make_chunk("src/main.rs", "+use std::collections::HashMap;")];
        let findings = scan_unused_imports(&chunks, std::path::Path::new("/nonexistent"), 10, &[]);
        assert!(
            findings.is_empty(),
            "should gracefully return empty without index"
        );
    }

    #[test]
    fn scan_dead_code_no_index_graceful() {
        let chunks = vec![make_chunk("src/main.rs", "+fn foo() {}")];
        let findings =
            scan_dead_code_in_review(&chunks, std::path::Path::new("/nonexistent"), 10, &[]);
        assert!(
            findings.is_empty(),
            "should gracefully return empty without index"
        );
    }

    #[test]
    fn scan_breaking_changes_no_index_graceful() {
        let chunks = vec![make_chunk("src/main.rs", "-pub fn important_api() {}")];
        let findings =
            scan_breaking_changes(&chunks, std::path::Path::new("/nonexistent"), 10, &[]);
        assert!(
            findings.is_empty(),
            "should gracefully return empty without index"
        );
    }

    #[test]
    fn scan_breaking_changes_detects_removed_pub_fn() {
        let chunks = vec![make_chunk(
            "src/lib.rs",
            "-pub fn important_api() {}\n+pub fn new_api() {}",
        )];
        // No index, so no callers detected — but the pattern should still compile
        let findings =
            scan_breaking_changes(&chunks, std::path::Path::new("/nonexistent"), 10, &[]);
        assert!(findings.is_empty(), "no index means no caller data");
    }

    // --- should_skip_file tests ---

    #[test]
    fn skip_empty_patterns() {
        assert!(!should_skip_file("src/main.ts", &[]));
        assert!(!should_skip_file("vitest.config.ts", &[]));
    }

    #[test]
    fn skip_exact_match() {
        let patterns = vec!["src/main.ts".into(), "src/index.ts".into()];
        assert!(should_skip_file("src/main.ts", &patterns));
        assert!(!should_skip_file("src/app.ts", &patterns));
    }

    #[test]
    fn skip_wildcard_suffix() {
        let patterns = vec!["*.config.ts".into()];
        assert!(should_skip_file("vitest.config.ts", &patterns));
        assert!(should_skip_file("vite.config.ts", &patterns));
        assert!(should_skip_file("webpack.config.ts", &patterns));
        assert!(!should_skip_file("src/app.ts", &patterns));
        assert!(!should_skip_file("config.ts", &patterns)); // basename = "config.ts", doesn't end with ".config.ts"
    }

    #[test]
    fn skip_wildcard_prefix() {
        let patterns = vec!["vite.config.*".into()];
        assert!(should_skip_file("vite.config.ts", &patterns));
        assert!(should_skip_file("vite.config.js", &patterns));
        assert!(!should_skip_file("webpack.config.ts", &patterns));
    }

    #[test]
    fn skip_doublestar_prefix() {
        let patterns = vec!["**/main.ts".into()];
        assert!(should_skip_file("src/main.ts", &patterns));
        assert!(should_skip_file("main.ts", &patterns));
        assert!(!should_skip_file("src/app.ts", &patterns));
    }

    #[test]
    fn skip_basename_exact() {
        // "src/main.ts" pattern — basename match works too
        let patterns = vec!["main.ts".into()];
        assert!(should_skip_file("src/main.ts", &patterns));
        assert!(should_skip_file("main.ts", &patterns));
        assert!(!should_skip_file("src/app.ts", &patterns));
    }

    #[test]
    fn skip_default_list_blocks_common_entry_points() {
        use crate::engine::rules::types::default_index_skip_files;
        let defaults = default_index_skip_files();

        // These should all be skipped
        assert!(should_skip_file("vitest.config.ts", &defaults));
        assert!(should_skip_file("vite.config.js", &defaults));
        assert!(should_skip_file("webpack.config.ts", &defaults));
        assert!(should_skip_file("src/main.ts", &defaults));
        assert!(should_skip_file("src/index.tsx", &defaults));
        assert!(should_skip_file("src/app.tsx", &defaults));

        // These should NOT be skipped
        assert!(!should_skip_file("src/lib.rs", &defaults));
        assert!(!should_skip_file("src/utils.ts", &defaults));
        assert!(!should_skip_file("src/components/Button.tsx", &defaults));
    }
}
