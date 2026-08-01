/// Rule engine — runs static rules against parsed diff chunks and merges findings
/// with LLM issues.
pub mod builtin;
pub mod matching;
pub mod types;

use tracing::debug;

use crate::engine::diff_parser::{DiffLineType, FileChunk, parse_diff};
use crate::engine::rules::types::{RuleFinding, RulesConfig};
use crate::engine::types::{ReviewIssue, Severity};

/// Map severity to a numeric rank for sorting (Critical=4, Major=3, Minor=2, Info=1).
fn severity_rank(sev: Severity) -> u8 {
    match sev {
        Severity::Critical => 4,
        Severity::Major => 3,
        Severity::Minor => 2,
        Severity::Info => 1,
    }
}

/// Run all rules (built-in + custom) against parsed diff chunks.
///
/// Returns findings capped by `config.max_findings`.
pub fn run_rules(chunks: &[FileChunk], config: &RulesConfig) -> Vec<RuleFinding> {
    if !config.enabled {
        debug!("rule engine is disabled");
        return Vec::new();
    }

    let mut all_rules = builtin::builtin_rules();
    all_rules.extend(config.custom_rules.clone());

    // Compile regex patterns once before the matching loop (Fix #41)
    for rule in &mut all_rules {
        rule.ensure_compiled();
    }

    debug!(
        rule_count = all_rules.len(),
        "running rules against diff chunks"
    );

    let mut findings = Vec::new();

    for file in chunks {
        let file_path = file
            .new_path
            .as_deref()
            .or(file.old_path.as_deref())
            .unwrap_or("unknown");

        for hunk in &file.chunks {
            for line in &hunk.lines {
                // Only check added lines (new code being introduced)
                if line.line_type != DiffLineType::Add {
                    continue;
                }

                for rule in &all_rules {
                    // Check language filter
                    if !matching::matches_language(rule, &file.language) {
                        continue;
                    }

                    // Check exclude filter
                    if matching::matches_exclude(rule, file_path) {
                        continue;
                    }

                    // Check pattern match
                    if !matching::match_rule_against_line(rule, &line.content) {
                        continue;
                    }

                    // Post-match filter (e.g., allow localhost URLs)
                    if builtin::post_match_filter(&rule.id, &line.content) {
                        continue;
                    }

                    let line_no = line.new_line_no.unwrap_or(0);

                    findings.push(RuleFinding {
                        rule_id: rule.id.clone(),
                        file: file_path.to_string(),
                        line: line_no,
                        severity: rule.severity,
                        title: format!("[{}] Rule: {}", rule.id, rule.id),
                        body: rule.message.clone(),
                    });
                }
            }
        }
    }

    // Sort by severity descending (Critical first) then by file + line
    findings.sort_by(|a, b| {
        let rank_a = severity_rank(a.severity);
        let rank_b = severity_rank(b.severity);
        rank_b
            .cmp(&rank_a)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    // Deduplicate: same rule_id + file + line
    findings.dedup_by(|a, b| a.rule_id == b.rule_id && a.file == b.file && a.line == b.line);

    // Cap at max_findings
    let capped = findings.len().min(config.max_findings);
    if findings.len() > capped {
        debug!(total = findings.len(), capped, "capping rule findings");
        findings.truncate(capped);
    }

    debug!(findings = findings.len(), "rule engine complete");
    findings
}

/// Merge rule-based findings with LLM-produced issues into a single list.
///
/// Rule findings are appended after LLM issues (LLM issues take priority).
/// Duplicates (same file + line) from rules are skipped if the LLM already
/// reported an issue for that location.
pub fn merge_rule_findings(
    llm_issues: Vec<ReviewIssue>,
    rule_findings: Vec<RuleFinding>,
) -> Vec<ReviewIssue> {
    let mut result = llm_issues;

    // Build a set of (file, line) pairs from LLM issues to avoid duplicates
    let llm_locations: std::collections::HashSet<(String, u32)> = result
        .iter()
        .filter_map(|issue| issue.line.map(|ln| (issue.file.clone(), ln)))
        .collect();

    for finding in rule_findings {
        // Skip if LLM already has an issue at the same file+line
        if llm_locations.contains(&(finding.file.clone(), finding.line)) {
            debug!(
                rule_id = %finding.rule_id,
                file = %finding.file,
                line = finding.line,
                "skipping rule finding (LLM already reported issue at this location)"
            );
            continue;
        }

        result.push(ReviewIssue {
            file: finding.file,
            line: Some(finding.line),
            severity: finding.severity,
            issue_type: Some("rule".to_string()),
            title: finding.title,
            body: finding.body,
            suggested_fix: None,
        });
    }

    result
}

/// Format rule findings as a context string for injection into the LLM prompt.
pub fn format_rule_context(findings: &[RuleFinding]) -> String {
    if findings.is_empty() {
        return String::new();
    }

    let mut ctx = String::from("Static rule engine findings (pre-verified):\n");
    ctx.push_str("---\n");

    for f in findings {
        ctx.push_str(&format!(
            "- [{}] {}:{} — {} ({}): {}",
            f.severity, f.file, f.line, f.rule_id, f.severity, f.body
        ));
        ctx.push('\n');
    }

    ctx.push_str("---\n");
    ctx
}

/// Convenience: parse diff + run rules in one step.
#[allow(dead_code)] // public API for future standalone usage
pub fn parse_and_run_rules(diff: &str, config: &RulesConfig) -> Vec<RuleFinding> {
    let chunks = parse_diff(diff);
    run_rules(&chunks, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;

    fn default_config() -> RulesConfig {
        RulesConfig {
            enabled: true,
            max_findings: 10,
            custom_rules: Vec::new(),
            index_skip_files: super::types::default_index_skip_files(),
        }
    }

    #[test]
    fn unwrap_rule_fires_on_rust_code() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
+    let x = something.unwrap();
 }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        let unwrap_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "bug-unwrap")
            .collect();
        assert!(
            !unwrap_findings.is_empty(),
            "should detect .unwrap() in added Rust code"
        );
    }

    #[test]
    fn todo_rule_fires() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
+    // TODO: fix this later
 }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        let todo_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "bug-todo")
            .collect();
        assert!(!todo_findings.is_empty(), "should detect TODO comment");
    }

    #[test]
    fn println_rule_fires_in_rust() {
        let diff = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,1 +1,2 @@
 pub fn greet() {
+    println!("hello");
 }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            findings.iter().any(|f| f.rule_id == "bug-println"),
            "should detect println! macro"
        );
    }

    #[test]
    fn console_log_rule_fires_in_ts() {
        let diff = r#"diff --git a/app.ts b/app.ts
--- a/app.ts
+++ b/app.ts
@@ -1,1 +1,2 @@
 function init() {
+    console.log("starting");
 }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            findings.iter().any(|f| f.rule_id == "bug-console-log"),
            "should detect console.log in TypeScript"
        );
    }

    #[test]
    fn language_filter_works() {
        // println! in a Python file should NOT fire bug-println (Rust only)
        let diff = r#"diff --git a/script.py b/script.py
--- a/script.py
+++ b/script.py
@@ -1,1 +1,2 @@
 def main():
+    println!("hello")
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            !findings.iter().any(|f| f.rule_id == "bug-println"),
            "bug-println should not fire on Python files"
        );
    }

    #[test]
    fn exclude_filter_works() {
        // unwrap in test/ directory should NOT fire
        let diff = r#"diff --git a/tests/integration.rs b/tests/integration.rs
--- a/tests/integration.rs
+++ b/tests/integration.rs
@@ -1,1 +1,2 @@
 #[test]
+fn test_something() { let _ = result.unwrap(); }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            !findings.iter().any(|f| f.rule_id == "bug-unwrap"),
            "bug-unwrap should not fire in tests/ directory"
        );
    }

    #[test]
    fn disabled_config_returns_no_findings() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
+    let x = something.unwrap();
 }
"#;
        let config = RulesConfig {
            enabled: false,
            ..default_config()
        };
        let findings = parse_and_run_rules(diff, &config);
        assert!(
            findings.is_empty(),
            "disabled config should produce no findings"
        );
    }

    #[test]
    fn max_findings_cap() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,8 @@
 fn main() {
+    let a = x.unwrap();
+    let b = y.clone();
+    let c = z.clone();
+    println!("debug");
+    // TODO: something
+    // FIXME: another
 }
"#;
        let config = RulesConfig {
            max_findings: 2,
            ..default_config()
        };
        let findings = parse_and_run_rules(diff, &config);
        assert!(findings.len() <= 2, "should cap findings at max_findings");
    }

    #[test]
    fn merge_skips_duplicates() {
        let llm = vec![ReviewIssue {
            file: "src/main.rs".to_string(),
            line: Some(5),
            severity: Severity::Minor,
            issue_type: Some("bug".to_string()),
            title: "Unnecessary unwrap".to_string(),
            body: "Use proper error handling".to_string(),
            suggested_fix: None,
        }];

        let rules = vec![RuleFinding {
            rule_id: "bug-unwrap".to_string(),
            file: "src/main.rs".to_string(),
            line: 5,
            severity: Severity::Minor,
            title: "[bug-unwrap] Rule: bug-unwrap".to_string(),
            body: "Can panic".to_string(),
        }];

        let merged = merge_rule_findings(llm, rules);
        // Should have 1 issue (LLM's), not 2 — rule finding at same location skipped
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn merge_appends_unique_rule_findings() {
        let llm = vec![ReviewIssue {
            file: "src/main.rs".to_string(),
            line: Some(5),
            severity: Severity::Minor,
            issue_type: Some("bug".to_string()),
            title: "Some issue".to_string(),
            body: "Details".to_string(),
            suggested_fix: None,
        }];

        let rules = vec![RuleFinding {
            rule_id: "bug-todo".to_string(),
            file: "src/main.rs".to_string(),
            line: 10,
            severity: Severity::Info,
            title: "[bug-todo] Rule: bug-todo".to_string(),
            body: "TODO found".to_string(),
        }];

        let merged = merge_rule_findings(llm, rules);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].issue_type.as_deref(), Some("rule"));
    }

    #[test]
    fn format_rule_context_non_empty() {
        let findings = vec![RuleFinding {
            rule_id: "bug-todo".to_string(),
            file: "src/lib.rs".to_string(),
            line: 42,
            severity: Severity::Info,
            title: "[bug-todo] Rule: bug-todo".to_string(),
            body: "TODO comment found".to_string(),
        }];
        let ctx = format_rule_context(&findings);
        assert!(ctx.contains("bug-todo"));
        assert!(ctx.contains("src/lib.rs:42"));
    }

    #[test]
    fn format_rule_context_empty() {
        let ctx = format_rule_context(&[]);
        assert!(ctx.is_empty());
    }

    #[test]
    fn hardcoded_secret_rule_fires() {
        let diff = r#"diff --git a/config.py b/config.py
--- a/config.py
+++ b/config.py
@@ -1,2 +1,3 @@
 # Config
+password = "super_secret_123"
 DB_HOST = "localhost"
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            findings.iter().any(|f| f.rule_id == "sec-hardcoded-secret"),
            "should detect hardcoded password"
        );
    }

    #[test]
    fn hardcoded_url_rule_fires() {
        let diff = r#"diff --git a/src/client.rs b/src/client.rs
--- a/src/client.rs
+++ b/src/client.rs
@@ -1,2 +1,3 @@
 fn get_url() -> &'static str {
+    "http://example.com/api/data"
 }
"#;
        let chunks = parse_diff(diff);
        eprintln!("DEBUG: {} chunks parsed", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            eprintln!(
                "DEBUG chunk[{}]: lang={}, new_path={:?}, hunks={}",
                i,
                c.language,
                c.new_path,
                c.chunks.len()
            );
            for (j, h) in c.chunks.iter().enumerate() {
                eprintln!("DEBUG   hunk[{}]: {} lines", j, h.lines.len());
                for l in &h.lines {
                    eprintln!("DEBUG     line: {:?} {:?}", l.line_type, l.content);
                }
            }
        }
        let findings = parse_and_run_rules(diff, &default_config());
        eprintln!("DEBUG: {} findings total", findings.len());
        for f in &findings {
            eprintln!("DEBUG finding: {:?}", f);
        }
        assert!(
            findings.iter().any(|f| f.rule_id == "sec-hardcoded-url"),
            "should detect non-localhost http:// URL"
        );
    }

    #[test]
    fn hardcoded_url_localhost_allowed() {
        let diff = r#"diff --git a/src/client.rs b/src/client.rs
--- a/src/client.rs
+++ b/src/client.rs
@@ -1,2 +1,3 @@
 fn get_url() -> &'static str {
+    "http://localhost:3000/health"
 }
"#;
        let findings = parse_and_run_rules(diff, &default_config());
        assert!(
            !findings.iter().any(|f| f.rule_id == "sec-hardcoded-url"),
            "localhost http URLs should be allowed"
        );
    }
}
