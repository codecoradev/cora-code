//! Rule-scoped inline suppression of findings (#554).
//!
//! A source comment containing `cora-ignore: <rule>[, <rule>...]` suppresses
//! findings whose title equals one of the listed rules (case-insensitive,
//! trimmed — the same identity `ignore.rules` uses for titles, but exact match
//! rather than substring so a marker cannot hide more than it names).
//!
//! * The marker applies to the line it is on, and, when the line contains only
//!   a comment (nothing but the comment syntax before the marker), also to the
//!   next line.
//! * The marker is matched as the substring `cora-ignore:` anywhere in a line,
//!   so it works after `//`, `#`, `--`, `/* */`, `<!-- -->`, and so on.
//! * A bare `cora-ignore` with no `:` and rule list never suppresses anything.
//!
//! One filter ([`apply`]) is applied to the merged deterministic + LLM findings.
//! It reads the post-change lines present in the parsed diff (added and
//! context lines), so no file I/O is needed. A marker that is not visible in
//! the diff (more than the context window away from the finding) is not seen.

use crate::engine::ReviewIssue;
use crate::engine::diff_parser::{DiffLineType, FileChunk};
use std::collections::{HashMap, HashSet};
use tracing::debug;

const MARKER: &str = "cora-ignore:";

/// Parse the rule list from a line carrying the marker, lowercased.
/// The marker is located on an ASCII-lowercased copy so byte offsets stay valid.
/// Returns an empty vec when there is no marker or no rule after it.
fn parse_rules(line: &str) -> Vec<String> {
    let lower = line.to_ascii_lowercase();
    let Some(idx) = lower.find(MARKER) else {
        return Vec::new();
    };
    let mut rest = &lower[idx + MARKER.len()..];
    // Drop a trailing block/HTML comment terminator.
    for end in ["*/", "-->"] {
        if let Some(i) = rest.find(end) {
            rest = &rest[..i];
        }
    }
    rest.split(',')
        .map(|r| r.trim().to_lowercase())
        .filter(|r| !r.is_empty())
        .collect()
}

/// True when everything before the marker is comment syntax / whitespace.
fn is_comment_only(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let Some(idx) = lower.find(MARKER) else {
        return false;
    };
    let before = line[..idx].trim();
    !before.is_empty()
        && before.chars().all(|c| {
            matches!(
                c,
                '/' | '*' | '#' | '-' | ';' | '<' | '!' | '%' | '"' | '\''
            ) || c.is_whitespace()
        })
}

/// Drop findings suppressed by an inline `cora-ignore:` marker.
pub fn apply(mut issues: Vec<ReviewIssue>, chunks: &[FileChunk]) -> Vec<ReviewIssue> {
    // (file, line) -> lowercased rules suppressed there.
    let mut suppressed: HashMap<(String, u32), HashSet<String>> = HashMap::new();
    for chunk in chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        for hunk in &chunk.chunks {
            for line in &hunk.lines {
                if line.line_type == DiffLineType::Remove {
                    continue;
                }
                let Some(ln) = line.new_line_no else { continue };
                let rules = parse_rules(&line.content);
                if rules.is_empty() {
                    if line.content.to_lowercase().contains("cora-ignore") {
                        debug!(
                            file = path,
                            line = ln,
                            "bare cora-ignore ignored: rule list required"
                        );
                    }
                    continue;
                }
                suppressed
                    .entry((path.to_string(), ln))
                    .or_default()
                    .extend(rules.iter().cloned());
                if is_comment_only(&line.content) {
                    suppressed
                        .entry((path.to_string(), ln + 1))
                        .or_default()
                        .extend(rules);
                }
            }
        }
    }
    if suppressed.is_empty() {
        return issues;
    }

    let before = issues.len();
    issues.retain(|issue| {
        let Some(ln) = issue.line else { return true };
        match suppressed.get(&(issue.file.clone(), ln)) {
            Some(rules) => !rules.contains(issue.title.trim().to_lowercase().as_str()),
            None => true,
        }
    });
    let dropped = before - issues.len();
    if dropped > 0 {
        debug!(dropped, "suppressed findings via inline cora-ignore");
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;
    use crate::engine::diff_parser::parse_diff;

    const RULE: &str = "Hardcoded password or secret in variable";

    fn issue(file: &str, line: u32, title: &str) -> ReviewIssue {
        ReviewIssue {
            file: file.to_string(),
            line: Some(line),
            severity: Severity::Major,
            issue_type: Some("rule".to_string()),
            title: title.to_string(),
            body: String::new(),
            suggested_fix: None,
        }
    }

    /// Diff adding `lines` to `f.rs` starting at line 1.
    fn diff_of(lines: &[&str]) -> Vec<FileChunk> {
        let mut d = format!(
            "diff --git a/f.rs b/f.rs\nnew file mode 100644\n--- /dev/null\n+++ b/f.rs\n@@ -0,0 +1,{} @@\n",
            lines.len()
        );
        for l in lines {
            d.push('+');
            d.push_str(l);
            d.push('\n');
        }
        parse_diff(&d)
    }

    #[test]
    fn same_line_suppresses() {
        let c = diff_of(&[
            "let bytesPerToken = 4; // cora-ignore: Hardcoded password or secret in variable",
        ]);
        assert!(apply(vec![issue("f.rs", 1, RULE)], &c).is_empty());
    }

    #[test]
    fn next_line_when_comment_only() {
        let c = diff_of(&[
            "// cora-ignore: Hardcoded password or secret in variable",
            "let x = 1;",
        ]);
        assert!(apply(vec![issue("f.rs", 2, RULE)], &c).is_empty());
    }

    #[test]
    fn next_line_not_suppressed_when_marker_trails_code() {
        let c = diff_of(&[
            "let a = 1; // cora-ignore: Hardcoded password or secret in variable",
            "let b = 2;",
        ]);
        assert_eq!(apply(vec![issue("f.rs", 2, RULE)], &c).len(), 1);
    }

    #[test]
    fn multiple_rules() {
        let c = diff_of(&["x(); // cora-ignore: Rule A, Rule B"]);
        let out = apply(
            vec![
                issue("f.rs", 1, "Rule A"),
                issue("f.rs", 1, "rule b"),
                issue("f.rs", 1, "Rule C"),
            ],
            &c,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Rule C");
    }

    #[test]
    fn unrelated_rule_and_other_lines_stay() {
        let c = diff_of(&["x(); // cora-ignore: Rule A", "y();"]);
        let out = apply(
            vec![issue("f.rs", 1, "Other"), issue("f.rs", 2, "Rule A")],
            &c,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn other_file_not_suppressed() {
        let c = diff_of(&["x(); // cora-ignore: Rule A"]);
        assert_eq!(apply(vec![issue("g.rs", 1, "Rule A")], &c).len(), 1);
    }

    #[test]
    fn bare_marker_ignored() {
        for l in [
            "x(); // cora-ignore",
            "x(); // cora-ignore:",
            "x(); // cora-ignore:  ,  ",
        ] {
            let c = diff_of(&[l]);
            assert_eq!(apply(vec![issue("f.rs", 1, "Rule A")], &c).len(), 1, "{l}");
        }
    }

    #[test]
    fn comment_styles() {
        for l in [
            "x = 1  # cora-ignore: Rule A",
            "x = 1 -- cora-ignore: Rule A",
            "x = 1; /* cora-ignore: Rule A */",
            "<p>hi</p> <!-- cora-ignore: Rule A -->",
        ] {
            let c = diff_of(&[l]);
            assert!(
                apply(vec![issue("f.rs", 1, "Rule A")], &c).is_empty(),
                "{l}"
            );
        }
    }

    #[test]
    fn block_comment_terminator_not_part_of_rule() {
        let c = diff_of(&["/* cora-ignore: Rule A, Rule B */", "z();"]);
        assert!(apply(vec![issue("f.rs", 2, "Rule B")], &c).is_empty());
    }

    #[test]
    fn case_insensitive_marker_and_rule() {
        let c = diff_of(&["x(); // CORA-IGNORE: HARDCODED PASSWORD OR SECRET IN VARIABLE"]);
        assert!(apply(vec![issue("f.rs", 1, RULE)], &c).is_empty());
    }

    #[test]
    fn non_ascii_before_marker_does_not_panic() {
        let c = diff_of(&["let s = \"\u{130}\u{130}\"; // cora-ignore: Rule A"]);
        assert!(apply(vec![issue("f.rs", 1, "Rule A")], &c).is_empty());
    }

    #[test]
    fn exact_title_not_substring() {
        let c = diff_of(&["x(); // cora-ignore: Hardcoded password"]);
        assert_eq!(apply(vec![issue("f.rs", 1, RULE)], &c).len(), 1);
    }

    /// Real scanner output flows through the same filter (#554).
    #[test]
    fn suppresses_real_scanner_finding() {
        let line = r#"const password = "hunter2hunter2";"#;
        let plain = diff_of(&[line]);
        let found = crate::engine::security_scanner::scan_security(&plain, 50);
        let found_all: Vec<_> = found
            .into_iter()
            .chain(crate::engine::secrets_scanner::scan_secrets(&plain, 50))
            .collect();
        assert!(!found_all.is_empty(), "scanner should flag the fixture");
        let title = found_all[0].title.clone();
        let issues = crate::engine::rules::merge_rule_findings(vec![], found_all);
        assert!(!issues.is_empty());

        let marked = diff_of(&[&format!("{line} // cora-ignore: {title}")]);
        let issues_marked: Vec<_> = issues
            .iter()
            .filter(|i| i.title == title)
            .cloned()
            .collect();
        assert!(apply(issues_marked, &marked).is_empty());
        // Without the marker the finding stays.
        assert!(!apply(issues, &plain).is_empty());
    }
}
