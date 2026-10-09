//! Finding post-processing shared by `cora review` and `cora scan` (#610).
//!
//! [`postprocess`] is the single place where raw findings (LLM issues plus
//! deterministic scanner findings) become the final, user-visible list. Fixing
//! a false-positive filter here fixes it for every command.
//!
//! The [`Source`] says what the findings were produced from:
//!
//! - [`Source::Diff`]: a parsed diff (`cora review`, including its
//!   LLM-failure fallback). Callers merge the deterministic findings
//!   (`Deterministic::merge_into`) before calling, because that step needs the
//!   index bridge and runs before the file-path hallucination guard.
//! - [`Source::Files`]: whole files (`cora scan`). Every line counts as added.
//!   Postprocessing itself runs the secrets and security scanners over the
//!   files and merges them into the issues (same-topic alias merge).
//!
//! Filter order and applicability:
//!
//! | # | Filter                            | Diff               | Files |
//! |---|-----------------------------------|--------------------|-------|
//! | 1 | scanner merge (secrets, security) | no (caller merges) | yes   |
//! | 2 | LLM hardcoded-secret FP check     | yes                | no    |
//! | 3 | Markdown fenced code block        | yes                | no    |
//! | 4 | `ignore.rules`                    | yes                | yes   |
//! | 5 | inline `cora-ignore:` markers     | yes                | yes   |
//! | 6 | context-line severity filter      | yes                | no    |
//!
//! Filters 2, 3 and 6 reason about added vs unchanged diff lines (or LLM
//! mistakes about added lines), which a full-file scan does not have: every
//! line of a scanned file is "added", and scan findings come from the
//! deterministic scanners rather than from an LLM judging a diff.

use tracing::debug;

use crate::config::schema::Config;
use crate::engine::diff_parser::FileChunk;
use crate::engine::scanner::{FileEntry, files_as_chunks};
use crate::engine::types::{ReviewIssue, Severity};
use crate::engine::{inline_suppress, rules, secrets_scanner, security_scanner};

/// What the findings being post-processed were produced from.
pub enum Source<'a> {
    /// A parsed diff (`cora review`).
    Diff(&'a [FileChunk]),
    /// Whole files (`cora scan`).
    Files(&'a [FileEntry]),
}

/// Run the shared post-processing pipeline over `issues`. See the module docs
/// for the per-source filter table.
pub fn postprocess(
    issues: Vec<ReviewIssue>,
    source: &Source<'_>,
    config: &Config,
) -> Vec<ReviewIssue> {
    match source {
        Source::Diff(chunks) => {
            let issues = apply_llm_secret_fp_filter(issues, chunks);
            let issues = apply_markdown_code_block_filter(issues, chunks);
            let issues = apply_ignore_rules(issues, &config.ignore.rules);
            // Rule-scoped inline suppression (`cora-ignore: <rule>`, #554),
            // shared by deterministic and LLM findings.
            let issues = inline_suppress::apply(issues, chunks);
            // Drop low-severity findings on unchanged (context) lines (#507).
            apply_context_line_filter(issues, chunks)
        }
        Source::Files(files) => {
            let chunks = files_as_chunks(files);
            let max = config.rules_config.max_findings;
            let mut merged = issues;
            for family in [
                secrets_scanner::scan_secrets(&chunks, max),
                security_scanner::scan_security(&chunks, max),
            ] {
                if !family.is_empty() {
                    merged = rules::merge_rule_findings(merged, family);
                }
            }
            let merged = apply_ignore_rules(merged, &config.ignore.rules);
            inline_suppress::apply(merged, &chunks)
        }
    }
}

/// Filter out LLM findings about hardcoded secrets/passwords that point to
/// diff lines which don't actually contain a literal string assignment.
///
/// The LLM sometimes flags struct field declarations like `api_key: String`
/// or `api_key: extract_api_key.clone()` as "Hardcoded password or secret in
/// variable". These are identifiers, not hardcoded values.
///
/// This function cross-validates each security finding against the actual
/// added line in the diff. If the line doesn't match the `sec-hardcoded-secret`
/// regex (i.e. no `password/key/secret = "literal"` pattern), the finding is
/// removed as a false positive.
fn apply_llm_secret_fp_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::diff_parser::DiffLineType;

    // Keywords that indicate an LLM finding is about hardcoded secrets.
    static SECRET_KEYWORDS: &[&str] = &[
        "hardcoded password",
        "hardcoded secret",
        "hardcoded credential",
        "hardcoded token",
        "hardcoded api key",
        "hardcoded api_key",
    ];

    // Pre-compute a lookup: (file_path, new_line_no) -> line content
    let added_lines: std::collections::HashMap<(String, u32), &str> = diff_chunks
        .iter()
        .flat_map(|chunk| {
            let path = chunk
                .new_path
                .as_deref()
                .or(chunk.old_path.as_deref())
                .unwrap_or("unknown");
            chunk.chunks.iter().flat_map(|hunk| {
                hunk.lines
                    .iter()
                    .filter(|l| l.line_type == DiffLineType::Add)
                    .filter_map(|l| {
                        l.new_line_no
                            .map(|ln| ((path.to_string(), ln), l.content.as_str()))
                    })
            })
        })
        .collect();

    let before = issues.len();
    issues.retain(|issue| {
        // Only check security-type findings about secrets
        let issue_type = issue.issue_type.as_deref().unwrap_or("");
        let title_lower = issue.title.to_lowercase();

        if issue_type != "security" {
            return true;
        }

        let is_secret_finding = SECRET_KEYWORDS.iter().any(|kw| title_lower.contains(kw));
        if !is_secret_finding {
            return true;
        }

        // Look up the actual diff line
        let line_num = issue.line.unwrap_or(0);
        let key = (issue.file.clone(), line_num);
        if let Some(actual_line) = added_lines.get(&key) {
            if !crate::engine::rules::builtin::has_secret_literal(actual_line, &issue.file) {
                debug!(
                    file = %issue.file,
                    line = line_num,
                    title = %issue.title,
                    "suppressed LLM false positive: line has no hardcoded secret literal"
                );
                return false; // Remove this finding
            }
        }

        // If we can't find the line (hallucinated path/line or context line),
        // keep the finding — better safe than sorry.
        true
    });

    let filtered = before - issues.len();
    if filtered > 0 {
        debug!(
            filtered,
            remaining = issues.len(),
            "filtered LLM false positives for hardcoded secret findings"
        );
    }

    issues
}

/// Drop findings located inside Markdown fenced code blocks (#329).
///
/// Code blocks in `.md`/`.mdx`/`.markdown` files are documentation examples,
/// not executable code — e.g. a `git push` inside a ```bash block must not be
/// flagged as SQL injection. Findings without a resolvable line, or in files
/// without any code block, are kept unchanged (safe default).
fn apply_markdown_code_block_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::markdown::{is_markdown, lines_inside_code_blocks};
    use std::collections::HashSet;

    // Build file path -> set of code-block line numbers, for markdown files only.
    let mut code_block_lines: std::collections::HashMap<String, HashSet<u32>> =
        std::collections::HashMap::new();
    for chunk in diff_chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        if !is_markdown(path) {
            continue;
        }
        let set = lines_inside_code_blocks(chunk);
        if !set.is_empty() {
            code_block_lines
                .entry(path.to_string())
                .or_default()
                .extend(set);
        }
    }

    if code_block_lines.is_empty() {
        return issues; // no markdown code blocks in this diff — fast path
    }

    let before = issues.len();
    issues.retain(|issue| {
        let Some(ln) = issue.line else {
            return true; // keep findings without a concrete line number
        };
        match code_block_lines.get(&issue.file) {
            Some(lines) => !lines.contains(&ln), // drop if inside a code block
            None => true,
        }
    });

    let dropped = before - issues.len();
    if dropped > 0 {
        debug!(
            dropped,
            remaining = issues.len(),
            "removed markdown code-block false positives"
        );
    }

    issues
}

/// Filter out issues whose `issue_type` matches any ignored rule pattern.
fn apply_ignore_rules(mut issues: Vec<ReviewIssue>, ignore_rules: &[String]) -> Vec<ReviewIssue> {
    if ignore_rules.is_empty() {
        return issues;
    }

    let before = issues.len();
    issues.retain(|issue| {
        !ignore_rules.iter().any(|pattern| {
            let pattern_lower = pattern.to_lowercase();
            let issue_type_lower = issue.issue_type.clone().unwrap_or_default().to_lowercase();
            let exact = pattern.trim().to_lowercase();
            let id_hit = |s: &str| s.trim().to_lowercase() == exact;
            issue_type_lower.contains(&pattern_lower)
                || issue.title.to_lowercase().contains(&pattern_lower)
                // Rule ids match exactly, including ids of scanner findings
                // merged into this issue (#597).
                || issue.rule_id.as_deref().is_some_and(id_hit)
                || issue.also_matches.iter().any(|a| id_hit(a))
        })
    });
    let filtered = before - issues.len();
    if filtered > 0 {
        debug!(
            filtered,
            remaining = issues.len(),
            rules = ignore_rules.len(),
            "filtered issues via ignore rules"
        );
    }

    issues
}

/// Drop findings on unchanged (context) or removed lines (#507 Pattern #3).
///
/// The LLM sometimes flags pre-existing code that appears in the diff purely
/// because surrounding lines changed. These findings are not about code the PR
/// introduces — they are noise.
///
/// **Policy:** Only drop `Minor` and `Info` severity findings on context/removed
/// lines. `Critical` and `Major` findings are kept regardless, because they may
/// represent real risks worth surfacing even in pre-existing code.
fn apply_context_line_filter(
    mut issues: Vec<ReviewIssue>,
    diff_chunks: &[FileChunk],
) -> Vec<ReviewIssue> {
    use crate::engine::diff_parser::DiffLineType;

    // Build lookup: (file, new_line_no) -> is_added
    // Only includes lines present in the diff (Add or Context). Lines not in
    // the diff at all are left alone (LLM line numbers can be imprecise).
    let mut line_kinds: std::collections::HashMap<(String, u32), DiffLineType> =
        std::collections::HashMap::new();
    for chunk in diff_chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        for hunk in &chunk.chunks {
            for line in &hunk.lines {
                if let Some(ln) = line.new_line_no {
                    line_kinds.insert((path.to_string(), ln), line.line_type);
                }
            }
        }
    }

    let before = issues.len();
    issues.retain(|issue| {
        // Keep findings without a concrete line number
        let Some(ln) = issue.line else {
            return true;
        };

        // Only filter if we can resolve this (file, line) to a diff line
        let Some(kind) = line_kinds.get(&(issue.file.clone(), ln)) else {
            return true; // not in diff — can't determine, keep
        };

        match kind {
            DiffLineType::Add => true, // genuinely new code — always keep
            DiffLineType::Context | DiffLineType::Remove => {
                // Pre-existing code — only keep if severity is high enough
                // Ord: Critical(0) < Major(1) < Minor(2) < Info(3)
                issue.severity <= Severity::Major
            }
        }
    });

    let dropped = before - issues.len();
    if dropped > 0 {
        debug!(
            dropped,
            remaining = issues.len(),
            "removed low-severity findings on unchanged diff context lines (#507)"
        );
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_fp_filter_removes_struct_field_declarations() {
        use crate::engine::diff_parser::*;

        // Simulate a diff with a struct field declaration (not a hardcoded secret)
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("crates/uteke-cli/src/cli.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 230,
                old_count: 0,
                new_start: 234,
                new_count: 2,
                header: "".to_string(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "    extract_api_key: Option<String>,".to_string(),
                        old_line_no: None,
                        new_line_no: Some(236),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "    extract_base_url: Option<String>,".to_string(),
                        old_line_no: None,
                        new_line_no: Some(237),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![
            ReviewIssue::new(
                "crates/uteke-cli/src/cli.rs",
                Some(236),
                Severity::Critical,
                "Hardcoded password or secret in variable",
            )
            .with_type("security")
            .with_body("Static security scanner detected..."),
        ];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert!(
            result.is_empty(),
            "struct field declaration should be filtered out"
        );
    }

    #[test]
    fn secret_fp_filter_keeps_actual_hardcoded_secrets() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/config.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 10,
                old_count: 0,
                new_start: 15,
                new_count: 1,
                header: "".to_string(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "    let api_key = \"sk-12345abcdef\";".to_string(),
                    old_line_no: None,
                    new_line_no: Some(15),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![
            ReviewIssue::new(
                "src/config.rs",
                Some(15),
                Severity::Critical,
                "Hardcoded password or secret in variable",
            )
            .with_type("security")
            .with_body("API key hardcoded..."),
        ];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "actual hardcoded secret should be kept");
    }

    /// LLM findings on the shapes added in #618-#620 are kept; SQL placeholders
    /// and references are still dropped.
    #[test]
    fn secret_fp_filter_matches_go_yaml_json_sql_shapes() {
        use crate::engine::diff_parser::*;

        let cases: &[(&str, bool)] = &[
            (r#"password := "hunter2hunter2""#, true),
            (r#"var apiKey string = "abcd1234efgh5678""#, true),
            ("password: hunter2hunter2", true),
            (r#"  "password": "hunter2hunter2","#, true),
            ("CREATE USER app WITH PASSWORD 'hunter2hunter2';", true),
            ("CREATE USER app WITH PASSWORD '%s';", false),
            ("password: required", false),
            (r#"  "password": "${DB_PASSWORD}","#, false),
        ];
        for (line, keep) in cases {
            let chunks = vec![FileChunk {
                old_path: None,
                new_path: Some("src/x.txt".to_string()),
                language: "txt".to_string(),
                chunks: vec![DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: 1,
                    header: "".to_string(),
                    lines: vec![DiffLine {
                        line_type: DiffLineType::Add,
                        content: line.to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    }],
                }],
                is_binary: false,
                is_deleted: false,
                is_new: false,
            }];
            let issues = vec![
                ReviewIssue::new(
                    "src/x.txt",
                    Some(1),
                    Severity::Critical,
                    "Hardcoded password in config",
                )
                .with_type("security"),
            ];
            let kept = apply_llm_secret_fp_filter(issues, &chunks).len() == 1;
            assert_eq!(kept, *keep, "{line}");
        }
    }

    #[test]
    fn secret_fp_filter_keeps_non_security_findings() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/main.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                header: "".to_string(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "    api_key: String,".to_string(),
                    old_line_no: None,
                    new_line_no: Some(1),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![
            ReviewIssue::new("src/main.rs", Some(1), Severity::Minor, "Use of unwrap()")
                .with_type("bugs")
                .with_body("This can panic"),
        ];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "non-security findings should pass through");
    }

    #[test]
    fn secret_fp_filter_keeps_findings_with_unknown_lines() {
        use crate::engine::diff_parser::*;

        // Empty diff — finding references a line not in the diff
        let diff_chunks: Vec<FileChunk> = vec![];

        let issues = vec![
            ReviewIssue::new(
                "src/config.rs",
                Some(999),
                Severity::Critical,
                "Hardcoded password or secret in variable",
            )
            .with_type("security")
            .with_body("..."),
        ];

        let result = apply_llm_secret_fp_filter(issues, &diff_chunks);
        assert_eq!(
            result.len(),
            1,
            "unknown lines should be kept (better safe than sorry)"
        );
    }

    #[test]
    fn ignore_rules_filters_by_title_match() {
        let issues = vec![
            ReviewIssue::new(
                "cli.rs",
                Some(236),
                Severity::Critical,
                "Command injection via exec/system with dynamic input",
            )
            .with_type("rule")
            .with_body("Static security scanner detected..."),
            ReviewIssue::new(
                "main.rs",
                Some(10),
                Severity::Major,
                "SQL injection via string concatenation",
            )
            .with_type("security")
            .with_body("..."),
        ];

        let rules = vec!["Command injection via exec/system with dynamic input".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "SQL injection via string concatenation");
    }

    #[test]
    fn ignore_rules_filters_by_issue_type_match() {
        let issues = vec![
            ReviewIssue::new("test.py", Some(50), Severity::Minor, "Some style issue")
                .with_type("style")
                .with_body("..."),
        ];

        let rules = vec!["style".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert!(result.is_empty());
    }

    #[test]
    fn ignore_rules_empty_keeps_all() {
        let issues = vec![
            ReviewIssue::new("f.rs", Some(1), Severity::Critical, "Any finding")
                .with_type("rule")
                .with_body("..."),
        ];

        let result = apply_ignore_rules(issues, &[]);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn ignore_rules_case_insensitive() {
        let issues = vec![
            ReviewIssue::new(
                "f.rs",
                Some(1),
                Severity::Critical,
                "HARDCODED password or SECRET in variable",
            )
            .with_type("rule")
            .with_body("..."),
        ];

        let rules = vec!["Hardcoded Password Or Secret".to_string()];
        let result = apply_ignore_rules(issues, &rules);
        assert!(result.is_empty());
    }

    // ─── #329: markdown fenced code-block false positives ───

    #[test]
    fn markdown_fp_filter_drops_finding_inside_code_block() {
        use crate::engine::diff_parser::*;

        // The exact #329 scenario: a `git push` inside a ```bash block in a
        // markdown doc, flagged as SQL injection.
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("AGENT.md".to_string()),
            language: "markdown".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 4,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```bash".to_string(),
                        old_line_no: None,
                        new_line_no: Some(167),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "git push origin vX.Y.Z".to_string(),
                        old_line_no: None,
                        new_line_no: Some(168),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```".to_string(),
                        old_line_no: None,
                        new_line_no: Some(169),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![
            ReviewIssue::new(
                "AGENT.md",
                Some(168),
                Severity::Critical,
                "SQL injection via string concatenation",
            )
            .with_type("security")
            .with_body("..."),
        ];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert!(
            result.is_empty(),
            "finding inside a markdown code block must be dropped"
        );
    }

    #[test]
    fn markdown_fp_filter_keeps_finding_outside_code_block() {
        use crate::engine::diff_parser::*;

        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("doc.md".to_string()),
            language: "markdown".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 3,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```bash".to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "echo hi".to_string(),
                        old_line_no: None,
                        new_line_no: Some(2),
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "```".to_string(),
                        old_line_no: None,
                        new_line_no: Some(3),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        // Finding on line 5 (outside the block, in prose) must survive.
        let issues = vec![
            ReviewIssue::new("doc.md", Some(5), Severity::Minor, "typo")
                .with_type("style")
                .with_body("..."),
        ];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "finding outside a code block must be kept");
    }

    #[test]
    fn markdown_fp_filter_keeps_findings_in_non_markdown_files() {
        use crate::engine::diff_parser::*;

        // A real .py file is never treated as markdown, even if it has ``` text.
        let diff_chunks = vec![FileChunk {
            old_path: None,
            new_path: Some("src/app.py".to_string()),
            language: "python".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 2,
                header: String::new(),
                lines: vec![DiffLine {
                    line_type: DiffLineType::Add,
                    content: "eval(request.body.code)".to_string(),
                    old_line_no: None,
                    new_line_no: Some(42),
                }],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];

        let issues = vec![
            ReviewIssue::new("src/app.py", Some(42), Severity::Critical, "eval injection")
                .with_type("security")
                .with_body("..."),
        ];

        let result = apply_markdown_code_block_filter(issues, &diff_chunks);
        assert_eq!(result.len(), 1, "non-markdown files are unaffected");
    }

    #[test]
    fn llm_secret_filter_keeps_typed_and_single_quoted_literals() {
        use crate::engine::diff_parser::{DiffHunk, DiffLine, DiffLineType, FileChunk};
        for (n, line) in [
            "const password: string = \"hunter2hunter2xx\";",
            "password: str = 'hunter2hunter2xx'",
            "let api_key = 'sk-hunter2hunter2xx';",
        ]
        .iter()
        .enumerate()
        {
            let chunks = vec![FileChunk {
                old_path: None,
                new_path: Some("src/a.ts".to_string()),
                language: "ts".to_string(),
                chunks: vec![DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: 1,
                    header: String::new(),
                    lines: vec![DiffLine {
                        line_type: DiffLineType::Add,
                        content: line.to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    }],
                }],
                is_binary: false,
                is_deleted: false,
                is_new: true,
            }];
            let issues = vec![
                ReviewIssue::new(
                    "src/a.ts",
                    Some(1),
                    Severity::Critical,
                    "Hardcoded password in source code",
                )
                .with_type("security"),
            ];
            assert_eq!(
                apply_llm_secret_fp_filter(issues, &chunks).len(),
                1,
                "case {n}: {line}"
            );
        }
    }

    #[test]
    fn ignore_rules_matches_rule_id_exactly() {
        let mut a = ReviewIssue::new("a.rs", Some(1), Severity::Major, "Plain title")
            .with_type("rule")
            .with_rule_id("sec-hardcoded-secret");
        let kept = apply_ignore_rules(vec![a.clone()], &["SEC-Hardcoded-Secret".to_string()]);
        assert!(kept.is_empty());
        // ids are exact, not substring
        let kept = apply_ignore_rules(vec![a.clone()], &["hardcoded".to_string()]);
        assert_eq!(kept.len(), 1);
        // merged-away scanner id
        a.rule_id = None;
        a.also_matches = vec!["sec-hardcoded-secret".to_string()];
        let kept = apply_ignore_rules(vec![a], &["sec-hardcoded-secret".to_string()]);
        assert!(kept.is_empty());
    }

    #[test]
    fn markdown_fp_filter_keeps_findings_without_line_number() {
        // Findings with no resolvable line are kept (safe default).
        let issues =
            vec![ReviewIssue::new("doc.md", None, Severity::Info, "vague").with_body("...")];

        let result = apply_markdown_code_block_filter(issues, &[]);
        assert_eq!(result.len(), 1);
    }

    // ─── postprocess() at the shared interface (#610) ───

    use crate::engine::diff_parser::parse_diff;

    fn issue(file: &str, line: Option<u32>, sev: Severity, ty: &str, title: &str) -> ReviewIssue {
        ReviewIssue::new(file.to_string(), line, sev, title.to_string()).with_type(ty.to_string())
    }

    fn file_entry(path: &str, content: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            content: content.to_string(),
            lines: content.lines().count(),
        }
    }

    fn py_diff(extra: &str) -> Vec<FileChunk> {
        parse_diff(&format!(
            "diff --git a/src/app.py b/src/app.py\n--- a/src/app.py\n+++ b/src/app.py\n@@ -1,2 +1,3 @@\n import os\n+password = \"hunter2hunter2\"{extra}\n print(1)\n"
        ))
    }

    fn py_file(extra: &str) -> FileEntry {
        file_entry(
            "src/app.py",
            &format!("import os\npassword = \"hunter2hunter2\"{extra}\nprint(1)\n"),
        )
    }

    fn scanner_title(config: &Config) -> String {
        let files = [py_file("")];
        let out = postprocess(Vec::new(), &Source::Files(&files), config);
        out.iter()
            .find(|i| i.line == Some(2))
            .expect("scanner finding on line 2")
            .title
            .clone()
    }

    #[test]
    fn files_source_runs_deterministic_scanners() {
        let files = [py_file("")];
        let out = postprocess(Vec::new(), &Source::Files(&files), &Config::default());
        assert!(
            out.iter()
                .any(|i| i.file == "src/app.py" && i.line == Some(2))
        );
    }

    #[test]
    fn files_source_honors_cora_ignore_marker() {
        let title = scanner_title(&Config::default());
        let files = [py_file(&format!("  # cora-ignore: {title}"))];
        let out = postprocess(Vec::new(), &Source::Files(&files), &Config::default());
        assert!(!out.iter().any(|i| i.line == Some(2)), "got {out:?}");
    }

    #[test]
    fn files_source_honors_ignore_rules() {
        let mut config = Config::default();
        config.ignore.rules = vec![scanner_title(&config)];
        let files = [py_file("")];
        let out = postprocess(Vec::new(), &Source::Files(&files), &config);
        assert!(!out.iter().any(|i| i.line == Some(2)), "got {out:?}");
    }

    #[test]
    fn files_source_aliases_scanner_finding_onto_same_topic_llm_issue() {
        let files = [py_file("")];
        let llm = vec![issue(
            "src/app.py",
            Some(2),
            Severity::Major,
            "security",
            "Hardcoded password in source",
        )];
        let out = postprocess(llm, &Source::Files(&files), &Config::default());
        let on_line: Vec<_> = out.iter().filter(|i| i.line == Some(2)).collect();
        assert_eq!(on_line.len(), 1, "got {out:?}");
        assert_eq!(on_line[0].title, "Hardcoded password in source");
        assert!(!on_line[0].also_matches.is_empty());
    }

    #[test]
    fn files_source_does_not_apply_diff_only_filters() {
        // Markdown code block: the diff filter would drop findings inside the
        // fence; a file scan keeps them.
        let md = file_entry(
            "README.md",
            "# Doc\n```py\npassword = \"hunter2hunter2\"\n```\n",
        );
        // LLM secret cross-check: a "hardcoded password" finding on a line
        // without a literal is dropped for diffs but kept for file scans.
        let src = file_entry("src/a.rs", "struct S {\n    api_key: String,\n}\n");
        let llm = vec![
            issue("README.md", Some(3), Severity::Major, "bug", "Doc example"),
            issue(
                "src/a.rs",
                Some(2),
                Severity::Major,
                "security",
                "Hardcoded password or secret",
            ),
        ];
        let files = [md, src];
        let out = postprocess(llm, &Source::Files(&files), &Config::default());
        assert!(out.iter().any(|i| i.title == "Doc example"), "got {out:?}");
        assert!(
            out.iter()
                .any(|i| i.title == "Hardcoded password or secret"),
            "got {out:?}"
        );
    }

    #[test]
    fn diff_source_honors_cora_ignore_marker() {
        let files = [py_file("")];
        let scanned = postprocess(Vec::new(), &Source::Files(&files), &Config::default());
        let title = scanned
            .iter()
            .find(|i| i.line == Some(2))
            .unwrap()
            .title
            .clone();
        let chunks = py_diff(&format!("  # cora-ignore: {title}"));
        let issues = vec![issue(
            "src/app.py",
            Some(2),
            Severity::Major,
            "rule",
            &title,
        )];
        let out = postprocess(issues, &Source::Diff(&chunks), &Config::default());
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn diff_source_honors_ignore_rules() {
        let chunks = py_diff("");
        let mut config = Config::default();
        config.ignore.rules = vec!["noisy".to_string()];
        let issues = vec![
            issue(
                "src/app.py",
                Some(2),
                Severity::Major,
                "style",
                "Noisy finding",
            ),
            issue(
                "src/app.py",
                Some(2),
                Severity::Major,
                "bug",
                "Real finding",
            ),
        ];
        let out = postprocess(issues, &Source::Diff(&chunks), &config);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Real finding");
    }

    #[test]
    fn diff_source_applies_diff_only_filters() {
        let chunks = py_diff("");
        let issues = vec![
            // line 1 is an unchanged context line: Minor dropped, Major kept
            issue(
                "src/app.py",
                Some(1),
                Severity::Minor,
                "style",
                "Minor on context",
            ),
            issue(
                "src/app.py",
                Some(1),
                Severity::Major,
                "bug",
                "Major on context",
            ),
            // line 2 is added and holds a literal: secret finding kept
            issue(
                "src/app.py",
                Some(2),
                Severity::Critical,
                "security",
                "Hardcoded password",
            ),
        ];
        let out = postprocess(issues, &Source::Diff(&chunks), &Config::default());
        let titles: Vec<_> = out.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Major on context", "Hardcoded password"]);
    }

    #[test]
    fn diff_source_drops_llm_secret_fp_on_added_line_without_literal() {
        let chunks = parse_diff(
            "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,1 +1,2 @@\n struct S {\n+    api_key: String,\n",
        );
        let issues = vec![issue(
            "src/a.rs",
            Some(2),
            Severity::Major,
            "security",
            "Hardcoded password or secret",
        )];
        let out = postprocess(issues, &Source::Diff(&chunks), &Config::default());
        assert!(out.is_empty(), "got {out:?}");
    }
}
