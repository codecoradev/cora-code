/// Rule engine — runs static rules against parsed diff chunks and merges findings
/// with LLM issues.
pub mod builtin;
pub mod matching;
pub mod types;

use tracing::debug;

use crate::engine::diff_parser::{FileChunk, parse_diff};
use crate::engine::rules::types::{CustomRule, RuleFinding, RulesConfig};
use crate::engine::scan_input::{self, ScanFile};
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

/// Run all rules (built-in + custom) against the added lines of parsed diff
/// chunks (diff adapter over [`run_rules_in`]).
///
/// Returns findings capped by `config.max_findings`.
pub fn run_rules(chunks: &[FileChunk], config: &RulesConfig) -> Vec<RuleFinding> {
    run_rules_in(&scan_input::from_chunks(chunks), config)
}

/// Run all rules (built-in + custom) against file lines. The one implementation
/// behind both the diff and the whole-file entry points.
/// Built-in rules that skip test code (files under test paths and inline Rust
/// test blocks) unless `rules_engine.include_tests` is set (#663): tests
/// legitimately unwrap, print, clone, use `http://` fixtures and fake secrets.
/// Real secrets in tests are still found by the high-confidence scanners.
const TEST_EXEMPT_RULES: &[&str] = &[
    "bug-unwrap",
    "bug-expect",
    "bug-println",
    "qual-clone",
    "qual-error-ignore",
    "sec-hardcoded-url",
    "sec-hardcoded-secret",
];

/// Lint custom rules: returns one message per problem (empty = fine).
///
/// Catches what would otherwise fail silently: a pattern that is not a valid
/// regex, an id that is empty, duplicated or shadows a built-in rule, an empty
/// message, and `languages` values that match no file (#684, #678).
pub fn validate_custom_rules(rules: &[CustomRule]) -> Vec<String> {
    let builtin_ids: std::collections::HashSet<String> =
        builtin::builtin_rules().into_iter().map(|r| r.id).collect();
    let mut seen = std::collections::HashSet::new();
    let mut errs = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        let name = if r.id.is_empty() {
            format!("rules_engine.custom[{i}]")
        } else {
            format!("rules_engine.custom[{i}] ({})", r.id)
        };
        if r.id.trim().is_empty() {
            errs.push(format!("{name}: id must not be empty"));
        } else if builtin_ids.contains(&r.id) {
            errs.push(format!("{name}: id collides with a built-in rule"));
        } else if !seen.insert(r.id.clone()) {
            errs.push(format!("{name}: duplicate id"));
        }
        if let Err(e) = regex::Regex::new(&r.pattern) {
            errs.push(format!("{name}: pattern is not a valid regex: {e}"));
        }
        if r.message.trim().is_empty() {
            errs.push(format!("{name}: message must not be empty"));
        }
        if r.languages.is_empty() {
            errs.push(format!(
                "{name}: languages is empty, so the rule can never match; use [\"all\"] or file extensions like [\"rs\"]"
            ));
        }
        for lang in &r.languages {
            if matching::normalize_language(lang).is_none() {
                errs.push(format!(
                    "{name}: languages entry \"{lang}\" is not a known extension or language (use e.g. \"rs\", \"py\", \"ts\", or \"all\")"
                ));
            }
        }
    }
    errs
}

pub fn run_rules_in(files: &[ScanFile<'_>], config: &RulesConfig) -> Vec<RuleFinding> {
    if !config.enabled {
        debug!("rule engine is disabled");
        return Vec::new();
    }

    let mut all_rules = builtin::builtin_rules();
    if !config.clone_hints {
        // `.clone()` hints are noise on idiomatic Rust (Arc::clone, handles for threads).
        all_rules.retain(|r| r.id != "qual-clone");
    }
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

    for file in files {
        let file_path = file.path;

        for &(line_no, content) in &file.lines {
            for rule in &all_rules {
                // Check language filter
                if !matching::matches_language(rule, file.language) {
                    continue;
                }

                // Check exclude filter
                if matching::matches_exclude(rule, file_path) {
                    continue;
                }

                // Test code is exempt from the noisy built-in rules (#663).
                if !config.include_tests
                    && file.in_test(line_no)
                    && TEST_EXEMPT_RULES.contains(&rule.id.as_str())
                {
                    continue;
                }

                // Check pattern match
                if !matching::match_rule_against_line(rule, content) {
                    continue;
                }

                // Post-match filter (e.g., allow localhost URLs)
                if builtin::post_match_filter_for_path(&rule.id, content, file_path) {
                    continue;
                }

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
    let capped = findings.len().min(config.finding_cap());
    if findings.len() > capped {
        debug!(total = findings.len(), capped, "capping rule findings");
        findings.truncate(capped);
    }

    debug!(findings = findings.len(), "rule engine complete");
    findings
}

/// Words that carry no topic (generic review vocabulary).
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "via",
    "from",
    "that",
    "this",
    "use",
    "using",
    "used",
    "rule",
    "bug",
    "sec",
    "possible",
    "potential",
    "detected",
    "found",
    "issue",
    "source",
    "code",
    "line",
    "file",
    "variable",
    "value",
    "committed",
    "missing",
    "unsafe",
    "should",
    "not",
    "are",
    "can",
    "may",
    "into",
    "stored",
    "exposed",
    "in",
    "of",
    "or",
    "an",
    "to",
    "is",
];

/// Topic words of a title / rule id: lowercase alphanumeric runs, plural `s`
/// dropped, generic vocabulary removed, and the secret family (password, key,
/// token, credential, secret) folded into one word, since reviewers and
/// scanners name the same hardcoded-secret problem differently.
fn topic_words(text: &str) -> std::collections::HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.strip_suffix('s').filter(|t| t.len() >= 3).unwrap_or(w))
        .filter(|w| !STOPWORDS.contains(w))
        .map(|w| match w {
            "password" | "passwd" | "pwd" | "credential" | "token" | "apikey" | "api_key"
            | "key" | "secret" => "secret".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// True when an LLM issue plausibly describes the same problem as a scanner
/// finding: they share at least one topic word (#609).
fn same_topic(issue: &ReviewIssue, finding: &RuleFinding) -> bool {
    let mut issue_words = topic_words(&issue.title);
    if let Some(kind) = &issue.issue_type {
        issue_words.extend(topic_words(kind));
    }
    let finding_words = topic_words(&format!("{} {}", finding.title, finding.rule_id));
    issue_words.iter().any(|w| finding_words.contains(w))
}

/// Merge rule-based findings with LLM-produced issues into a single list.
///
/// Rule findings are appended after LLM issues (LLM issues take priority). A
/// rule finding is skipped as a duplicate only when an LLM issue on the same
/// file+line is about the same topic (see [`same_topic`]); its id and title are
/// then recorded on that issue's `also_matches` (not serialized) so an inline
/// `cora-ignore:` / `ignore.rules` entry naming the scanner rule still
/// suppresses it (#597). An unrelated LLM issue on the same line is neither
/// aliased nor allowed to absorb the rule finding, so a marker naming one rule
/// cannot hide a different problem (#609).
pub fn merge_rule_findings(
    llm_issues: Vec<ReviewIssue>,
    rule_findings: Vec<RuleFinding>,
) -> Vec<ReviewIssue> {
    let mut result = llm_issues;

    // (file, line) -> indexes of the pre-existing issues at that location.
    let mut at_location: std::collections::HashMap<(String, u32), Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, issue) in result.iter().enumerate() {
        if let Some(ln) = issue.line {
            at_location
                .entry((issue.file.clone(), ln))
                .or_default()
                .push(idx);
        }
    }

    for finding in rule_findings {
        let holder = at_location
            .get(&(finding.file.clone(), finding.line))
            .and_then(|idxs| {
                idxs.iter()
                    .copied()
                    .find(|&i| same_topic(&result[i], &finding))
            });
        if let Some(idx) = holder {
            let holder = &mut result[idx];
            // The judge's issue stands in for the scanner finding, so it must
            // never be weaker than it: a judge (or a prompt-injected model)
            // that reports the same line as "info" cannot downgrade a
            // CRITICAL scanner finding, and the surviving issue carries the
            // scanner's rule id (so the cap and rule-scoped suppression see it).
            holder.severity = holder.severity.min(finding.severity);
            if holder.rule_id.is_none() {
                holder.rule_id = Some(finding.rule_id.clone());
            }
            holder.also_matches.push(finding.rule_id.clone());
            holder.also_matches.push(finding.title.clone());
            debug!(
                rule_id = %finding.rule_id,
                file = %finding.file,
                line = finding.line,
                "skipping rule finding (LLM already reported the same problem at this location)"
            );
            continue;
        }

        result.push(
            ReviewIssue::new(
                finding.file,
                Some(finding.line),
                finding.severity,
                finding.title,
            )
            .with_type("rule")
            .with_body(finding.body)
            .with_rule_id(finding.rule_id),
        );
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

    #[test]
    fn qual_clone_is_opt_in() {
        let entry = file_entry("src/lib.rs", "fn f(a: &Arc<S>) { let b = a.clone(); }\n");
        let off = rule_ids(&entry, &default_config());
        assert!(!off.iter().any(|i| i.starts_with("qual-clone")), "{off:?}");
        let mut cfg = default_config();
        cfg.clone_hints = true;
        let on = rule_ids(&entry, &cfg);
        assert!(on.iter().any(|i| i.starts_with("qual-clone")), "{on:?}");
    }

    fn rule(id: &str, pattern: &str, languages: &[&str]) -> CustomRule {
        CustomRule {
            id: id.into(),
            pattern: pattern.into(),
            severity: Severity::Major,
            message: "m".into(),
            languages: languages.iter().map(|s| s.to_string()).collect(),
            exclude: vec![],
            compiled_pattern: None,
        }
    }

    #[test]
    fn a_valid_custom_rule_has_no_problems() {
        assert!(validate_custom_rules(&[rule("mine", "fn a", &["rs"])]).is_empty());
        assert!(
            validate_custom_rules(&[rule("mine", "x", &["rust", "TypeScript", "*"])]).is_empty()
        );
    }

    #[test]
    fn validation_reports_each_silent_failure_with_the_rule_id() {
        let errs = validate_custom_rules(&[
            rule("bad", "(", &["rs"]),
            rule("lang", "x", &["klingon"]),
            rule("lang", "x", &[]),
            rule("bug-todo", "x", &["rs"]),
        ]);
        let all = errs.join("\n");
        assert!(
            all.contains("(bad)") && all.contains("not a valid regex"),
            "{all}"
        );
        assert!(all.contains("klingon"), "{all}");
        assert!(all.contains("duplicate id"), "{all}");
        assert!(all.contains("languages is empty"), "{all}");
        assert!(all.contains("collides with a built-in rule"), "{all}");
    }

    #[test]
    fn language_names_match_files_like_extensions() {
        let r = rule("mine", "fn a", &["rust"]);
        assert!(matching::matches_language(&r, "rs"));
        assert!(!matching::matches_language(&r, "py"));
        assert!(matching::matches_language(&rule("m", "x", &["tsx"]), "ts"));
    }

    #[test]
    fn exclude_and_languages_are_optional_in_yaml() {
        let r: CustomRule = serde_yaml_ng::from_str(
            "id: no-console-log\npattern: 'console'\nseverity: minor\nmessage: no\n",
        )
        .unwrap();
        assert!(r.exclude.is_empty());
        assert_eq!(r.languages, vec!["all".to_string()]);
    }

    fn default_config() -> RulesConfig {
        RulesConfig {
            enabled: true,
            max_findings: 10,
            custom_rules: Vec::new(),
            index_skip_files: super::types::default_index_skip_files(),
            include_tests: false,
            clone_hints: false,
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

    fn llm_issue(line: u32, title: &str, kind: Option<&str>) -> ReviewIssue {
        let issue = ReviewIssue::new("src/a.rs", Some(line), Severity::Major, title);
        match kind {
            Some(k) => issue.with_type(k),
            None => issue,
        }
    }

    fn secret_finding(line: u32) -> RuleFinding {
        RuleFinding {
            rule_id: "crypto/hardcoded-secret".to_string(),
            file: "src/a.rs".to_string(),
            line,
            severity: Severity::Critical,
            title: "Hardcoded password or secret in variable".to_string(),
            body: String::new(),
        }
    }

    #[test]
    fn same_topic_matches_secret_family_wording() {
        for t in [
            "Hardcoded password stored in source code",
            "Hardcoded API key committed",
            "Plaintext credentials in config",
            "Weak, low-entropy secret",
            "Token exposed in source",
        ] {
            assert!(
                same_topic(&llm_issue(1, t, None), &secret_finding(1)),
                "{t}"
            );
        }
    }

    #[test]
    fn unrelated_llm_issue_neither_absorbs_nor_aliases_rule_finding() {
        // #609: SQL injection on the same line as a hardcoded secret.
        let merged = merge_rule_findings(
            vec![llm_issue(
                5,
                "SQL injection via string concatenation",
                Some("security"),
            )],
            vec![secret_finding(5)],
        );
        assert_eq!(merged.len(), 2);
        assert!(merged[0].also_matches.is_empty());
        assert_eq!(
            merged[1].rule_id.as_deref(),
            Some("crypto/hardcoded-secret")
        );
    }

    #[test]
    fn alias_goes_to_the_related_issue_not_the_first_one() {
        let merged = merge_rule_findings(
            vec![
                llm_issue(5, "SQL injection via string concatenation", None),
                llm_issue(5, "Hardcoded password stored in source code", None),
            ],
            vec![secret_finding(5)],
        );
        assert_eq!(merged.len(), 2);
        assert!(merged[0].also_matches.is_empty());
        assert!(
            merged[1]
                .also_matches
                .contains(&"crypto/hardcoded-secret".to_string())
        );
    }

    #[test]
    fn merge_skips_duplicates() {
        let llm = vec![
            ReviewIssue::new(
                "src/main.rs",
                Some(5),
                Severity::Minor,
                "Unnecessary unwrap",
            )
            .with_type("bug")
            .with_body("Use proper error handling"),
        ];

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
        // ...but its id and title stay addressable for suppression (#597).
        assert!(merged[0].also_matches.contains(&"bug-unwrap".to_string()));
        assert!(
            merged[0]
                .also_matches
                .contains(&"[bug-unwrap] Rule: bug-unwrap".to_string())
        );
        // The surviving issue carries the scanner's id: it stands in for the
        // finding, so the cap and rule-scoped suppression must see it.
        assert_eq!(merged[0].rule_id.as_deref(), Some("bug-unwrap"));
    }

    #[test]
    fn merge_appends_unique_rule_findings() {
        let llm = vec![
            ReviewIssue::new("src/main.rs", Some(5), Severity::Minor, "Some issue")
                .with_type("bug")
                .with_body("Details"),
        ];

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

    #[test]
    fn a_judge_issue_cannot_downgrade_the_scanner_finding_it_absorbs() {
        // Same line, same topic: the judge's "info" stands in for the
        // CRITICAL secret finding, so it must not be weaker than it.
        let mut weak = llm_issue(3, "Hardcoded password naming nit", Some("security"));
        weak.severity = Severity::Info;
        let out = merge_rule_findings(vec![weak], vec![secret_finding(3)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Critical);
        assert_eq!(out[0].rule_id.as_deref(), Some("crypto/hardcoded-secret"));
    }

    #[test]
    fn a_stronger_judge_issue_keeps_its_severity() {
        let mut strong = llm_issue(3, "Hardcoded password stored in source", Some("security"));
        strong.severity = Severity::Critical;
        let mut finding = secret_finding(3);
        finding.severity = Severity::Major;
        let out = merge_rule_findings(vec![strong], vec![finding]);
        assert_eq!(out[0].severity, Severity::Critical);
    }

    // ─── #663: test-code awareness ───

    fn file_entry(path: &str, content: &str) -> crate::engine::scanner::FileEntry {
        crate::engine::scanner::FileEntry {
            path: path.to_string(),
            lines: content.lines().count(),
            content: content.to_string(),
        }
    }

    fn rule_ids(entry: &crate::engine::scanner::FileEntry, cfg: &RulesConfig) -> Vec<String> {
        let files = crate::engine::scan_input::from_entries(std::slice::from_ref(entry));
        run_rules_in(&files, cfg)
            .into_iter()
            .map(|f| format!("{}@{}", f.rule_id, f.line))
            .collect()
    }

    const TODO_SRC: &str = "\
pub mod todo;
fn f() { let todo_state = 1; let _ = todo_state; }
/// the todo list is per session
fn g() {}
// TODO(me): real marker
// todo lower case is prose
// FIXME: also real
# HACK in a hash comment
";

    #[test]
    fn bug_todo_flags_only_deliberate_markers() {
        let entry = file_entry("src/a.rs", TODO_SRC);
        let ids: Vec<String> = rule_ids(&entry, &default_config())
            .into_iter()
            .filter(|i| i.starts_with("bug-todo"))
            .collect();
        assert_eq!(
            ids,
            vec!["bug-todo@5", "bug-todo@7", "bug-todo@8"],
            "{ids:?}"
        );
    }

    #[test]
    fn bug_println_ignores_stderr_macros_but_keeps_stdout_ones() {
        let src = "fn a(){ println!(\"x\"); }\nfn b(){ eprintln!(\"ok\"); }\nfn c(){ print!(\"y\"); }\nfn d(){ eprint!(\"z\"); }\n";
        let entry = file_entry("src/lib.rs", src);
        let ids: Vec<String> = rule_ids(&entry, &default_config())
            .into_iter()
            .filter(|i| i.starts_with("bug-println"))
            .collect();
        assert_eq!(ids, vec!["bug-println@1", "bug-println@3"], "{ids:?}");
    }

    #[test]
    fn bug_expect_flags_an_empty_message_but_not_a_stated_invariant() {
        let src = "fn a(){ c.stdout.take().expect(\"stdout piped above\"); }\nfn b(){ x.expect(\"\"); }\nfn c(){ y.unwrap(); }\n";
        let entry = file_entry("src/lib.rs", src);
        let ids = rule_ids(&entry, &default_config());
        assert!(!ids.contains(&"bug-expect@1".to_string()), "{ids:?}");
        assert!(ids.contains(&"bug-expect@2".to_string()), "{ids:?}");
        assert!(ids.contains(&"bug-unwrap@3".to_string()), "{ids:?}");
    }

    const RUST_WITH_TESTS: &str = "\
fn real() {
    let a = x.unwrap();
}
#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let b = y.unwrap();
        println!(\"debug\");
        let u = \"http://example.test/x\";
    }
}
";

    #[test]
    fn inline_rust_test_blocks_skip_the_noisy_rules() {
        let entry = file_entry("src/lib.rs", RUST_WITH_TESTS);
        let ids = rule_ids(&entry, &default_config());
        assert!(ids.contains(&"bug-unwrap@2".to_string()), "{ids:?}");
        assert!(
            !ids.iter()
                .any(|i| i.ends_with("@8") || i.ends_with("@9") || i.ends_with("@10")),
            "test block lines must be exempt: {ids:?}"
        );
    }

    #[test]
    fn include_tests_scans_test_blocks_again() {
        let entry = file_entry("src/lib.rs", RUST_WITH_TESTS);
        let cfg = RulesConfig {
            include_tests: true,
            ..default_config()
        };
        let ids = rule_ids(&entry, &cfg);
        assert!(ids.contains(&"bug-unwrap@8".to_string()), "{ids:?}");
        assert!(ids.contains(&"bug-println@9".to_string()), "{ids:?}");
    }

    #[test]
    fn test_paths_are_exempt_beyond_the_tests_directory() {
        // `foo_test.rs` / `app.test.ts` are not under tests/, but are test files.
        for path in ["src/foo_test.rs", "src/app.test.ts", "src/__tests__/a.rs"] {
            let entry = file_entry(path, "fn a() { let x = y.unwrap(); }\n");
            let ids = rule_ids(&entry, &default_config());
            assert!(
                !ids.iter().any(|i| i.starts_with("bug-unwrap")),
                "{path}: {ids:?}"
            );
        }
        let entry = file_entry("src/prod.rs", "fn a() { let x = y.unwrap(); }\n");
        assert!(
            rule_ids(&entry, &default_config())
                .iter()
                .any(|i| i.starts_with("bug-unwrap"))
        );
    }

    #[test]
    fn rules_that_are_not_noise_still_fire_in_tests() {
        // bug-todo is not exempt: a TODO in a test is still a TODO.
        let entry = file_entry("src/foo_test.rs", "// TODO: finish\n");
        assert!(
            rule_ids(&entry, &default_config())
                .iter()
                .any(|i| i.starts_with("bug-todo"))
        );
    }

    #[test]
    fn println_is_fine_in_program_entry_points_only() {
        let src = "fn main() { println!(\"hi\"); }\n";
        for ok in [
            "src/main.rs",
            "crates/x/src/bin/tool.rs",
            "examples/demo.rs",
        ] {
            let entry = file_entry(ok, src);
            assert!(
                !rule_ids(&entry, &default_config())
                    .iter()
                    .any(|i| i.starts_with("bug-println")),
                "{ok}"
            );
        }
        let entry = file_entry("src/lib.rs", "pub fn f() { println!(\"hi\"); }\n");
        assert!(
            rule_ids(&entry, &default_config())
                .iter()
                .any(|i| i.starts_with("bug-println"))
        );
    }
}
