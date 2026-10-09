//! Rule-scoped inline suppression of findings (#554).
//!
//! A source comment containing `cora-ignore: <rule>[, <rule>...]` suppresses
//! findings whose title **or rule id** equals one of the listed rules
//! (case-insensitive, trimmed, exact match rather than substring so a marker
//! cannot hide more than it names). A finding also answers to the ids/titles in
//! its `also_matches`: scanner findings dropped in merge because an LLM issue
//! sat on the same line (#597).
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
use crate::engine::diff_parser::FileChunk;
use crate::engine::scan_input::ScanFile;
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

/// True when the finding's title, rule id, or merged-away alias is in `rules`
/// (lowercased set).
fn matches_any(issue: &ReviewIssue, rules: &HashSet<String>) -> bool {
    let hit = |s: &str| rules.contains(s.trim().to_lowercase().as_str());
    hit(&issue.title)
        || issue.rule_id.as_deref().is_some_and(hit)
        || issue.also_matches.iter().any(|a| hit(a))
}

/// Drop findings suppressed by an inline `cora-ignore:` marker, reading the
/// post-change lines of a diff (diff adapter over [`apply_lines`]).
pub fn apply(issues: Vec<ReviewIssue>, chunks: &[FileChunk]) -> Vec<ReviewIssue> {
    let files: Vec<ScanFile<'_>> = chunks.iter().map(ScanFile::post_image).collect();
    apply_lines(issues, &files)
}

/// Drop findings suppressed by an inline `cora-ignore:` marker found in
/// `files`' lines.
pub fn apply_lines(mut issues: Vec<ReviewIssue>, files: &[ScanFile<'_>]) -> Vec<ReviewIssue> {
    // (file, line) -> lowercased rules suppressed there.
    let mut suppressed: HashMap<(String, u32), HashSet<String>> = HashMap::new();
    for file in files {
        let path = file.path;
        for &(ln, content) in &file.lines {
            let rules = parse_rules(content);
            if rules.is_empty() {
                if content.to_lowercase().contains("cora-ignore") {
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
            if is_comment_only(content) {
                suppressed
                    .entry((path.to_string(), ln + 1))
                    .or_default()
                    .extend(rules);
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
            Some(rules) => !matches_any(issue, rules),
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
        ReviewIssue::new(
            file.to_string(),
            Some(line),
            Severity::Major,
            title.to_string(),
        )
        .with_type("rule")
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

    fn with_id(mut i: ReviewIssue, id: &str) -> ReviewIssue {
        i.rule_id = Some(id.to_string());
        i
    }

    #[test]
    fn marker_matches_rule_id() {
        let c = diff_of(&["x(); // cora-ignore: sec-hardcoded-secret"]);
        let i = with_id(issue("f.rs", 1, RULE), "sec-hardcoded-secret");
        assert!(apply(vec![i], &c).is_empty());
    }

    #[test]
    fn marker_matches_title_when_rule_id_present() {
        let c = diff_of(&["x(); // cora-ignore: Hardcoded password or secret in variable"]);
        let i = with_id(issue("f.rs", 1, RULE), "sec-hardcoded-secret");
        assert!(apply(vec![i], &c).is_empty());
    }

    #[test]
    fn rule_id_match_is_case_insensitive_and_exact() {
        let c = diff_of(&["x(); // cora-ignore:  SEC-Hardcoded-Secret "]);
        let i = with_id(issue("f.rs", 1, RULE), "sec-hardcoded-secret");
        assert!(apply(vec![i], &c).is_empty());
        let c = diff_of(&["x(); // cora-ignore: sec-hardcoded"]);
        let i = with_id(issue("f.rs", 1, RULE), "sec-hardcoded-secret");
        assert_eq!(apply(vec![i], &c).len(), 1);
    }

    #[test]
    fn other_rule_id_does_not_suppress() {
        let c = diff_of(&["x(); // cora-ignore: sec-other"]);
        let i = with_id(issue("f.rs", 1, RULE), "sec-hardcoded-secret");
        assert_eq!(apply(vec![i], &c).len(), 1);
    }

    /// A scanner finding dropped in merge (LLM issue on the same line) must
    /// still be addressable by its id and title (#597).
    #[test]
    fn marker_naming_merged_away_scanner_rule_suppresses_llm_issue() {
        use crate::engine::rules::{merge_rule_findings, types::RuleFinding};
        let scanner = || RuleFinding {
            rule_id: "sec-hardcoded-secret".to_string(),
            file: "f.rs".to_string(),
            line: 1,
            severity: Severity::Major,
            title: RULE.to_string(),
            body: String::new(),
        };
        let llm = || issue("f.rs", 1, "Hardcoded password stored in source code");
        let merged = merge_rule_findings(vec![llm()], vec![scanner()]);
        assert_eq!(merged.len(), 1, "scanner finding is deduped away");

        for marker in ["sec-hardcoded-secret", RULE] {
            let c = diff_of(&[&format!("x(); // cora-ignore: {marker}")]);
            assert!(apply(merged.clone(), &c).is_empty(), "{marker}");
        }
        // An unrelated marker still leaves it.
        let c = diff_of(&["x(); // cora-ignore: something-else"]);
        assert_eq!(apply(merged, &c).len(), 1);
    }

    /// #609: a marker naming a scanner rule must not hide an unrelated LLM
    /// finding on the same line.
    #[test]
    fn marker_naming_scanner_rule_does_not_hide_unrelated_llm_issue() {
        use crate::engine::rules::{merge_rule_findings, types::RuleFinding};
        let scanner = RuleFinding {
            rule_id: "sec-hardcoded-secret".to_string(),
            file: "f.rs".to_string(),
            line: 1,
            severity: Severity::Major,
            title: RULE.to_string(),
            body: String::new(),
        };
        let sqli = issue("f.rs", 1, "SQL injection via string concatenation");
        let merged = merge_rule_findings(vec![sqli], vec![scanner]);
        assert_eq!(merged.len(), 2, "unrelated findings stay separate");
        let c = diff_of(&["q(\"..\" + pw); // cora-ignore: sec-hardcoded-secret"]);
        let left = apply(merged, &c);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].title, "SQL injection via string concatenation");
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
