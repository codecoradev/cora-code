//! The deterministic half of a review: every check that needs no LLM.
//!
//! [`run`] takes one input (parsed diff chunks, the config, the index bridge)
//! and returns a [`DeterministicReport`] with the findings of each scanner
//! family plus the helpers the orchestrator needs: the context text for the
//! LLM prompt ([`DeterministicReport::context`]) and the merge of the findings
//! into a list of issues ([`DeterministicReport::merge_into`]).
//!
//! It always operates on the ORIGINAL (unsanitized) diff chunks — only the LLM
//! sees sanitized text (ALIBI defense, arXiv:2607.24964). It never calls the
//! LLM and never touches the network, so it can be tested end to end with an
//! in-memory [`IndexBridge`].
//!
//! Family order is part of the output contract (context text and merge order):
//! rules, secrets, security, index unused imports, index dead code, index
//! breaking changes.

use crate::config::schema::Config;
use crate::engine::ReviewIssue;
use crate::engine::comment_sanitizer::{self, SanitizeReport};
use crate::engine::diff_parser::FileChunk;
use crate::engine::index_bridge::IndexBridge;
use crate::engine::rules::{self, types::RuleFinding};
use crate::engine::{index_scanner, secrets_scanner, security_scanner};

/// Findings of every deterministic scanner family, in contract order.
#[derive(Debug, Default)]
pub struct DeterministicReport {
    pub rules: Vec<RuleFinding>,
    pub secrets: Vec<RuleFinding>,
    pub security: Vec<RuleFinding>,
    pub index_unused: Vec<RuleFinding>,
    pub index_dead: Vec<RuleFinding>,
    pub index_breaking: Vec<RuleFinding>,
    /// Unverified-claim flags found in added comments (not findings).
    pub claims: SanitizeReport,
}

/// Exclusion patterns for review-time index scanners: the exact set the
/// indexer uses (`ignore.files` + `index_skip_files`), so review and index
/// never disagree about which files are out of scope.
pub fn skip_patterns(config: &Config) -> Vec<String> {
    crate::index::skip_patterns_from_config(Some(config)).unwrap_or_default()
}

/// Run every deterministic check on a parsed diff.
pub fn run(chunks: &[FileChunk], config: &Config, bridge: &IndexBridge) -> DeterministicReport {
    let max = config.rules_config.max_findings;
    let skip = skip_patterns(config);

    DeterministicReport {
        rules: rules::run_rules(chunks, &config.rules_config),
        secrets: secrets_scanner::scan_secrets(chunks, max),
        security: security_scanner::scan_security(chunks, max),
        index_unused: index_scanner::scan_unused_imports(bridge, chunks, max, &skip),
        index_dead: index_scanner::scan_dead_code_in_review(bridge, chunks, max, &skip),
        index_breaking: index_scanner::scan_breaking_changes(bridge, chunks, max, &skip),
        claims: comment_sanitizer::flag_claims(chunks),
    }
}

impl DeterministicReport {
    fn families(&self) -> [&Vec<RuleFinding>; 6] {
        [
            &self.rules,
            &self.secrets,
            &self.security,
            &self.index_unused,
            &self.index_dead,
            &self.index_breaking,
        ]
    }

    /// Total number of findings across all families.
    pub fn len(&self) -> usize {
        self.families().iter().map(|f| f.len()).sum()
    }

    /// True when no scanner produced a finding.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Context text for the LLM prompt, or `None` when there is nothing to say.
    ///
    /// Sections, in order: optional static-analysis output, the unverified
    /// claim warning, then one formatted block per non-empty family; joined
    /// with a blank line.
    pub fn context(&self, static_context: Option<&str>) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(sa) = static_context {
            parts.push(sa.to_string());
        }
        if let Some(warning) = comment_sanitizer::format_claim_warning(&self.claims) {
            parts.push(warning);
        }
        for family in self.families() {
            let text = rules::format_rule_context(family);
            if !text.is_empty() {
                parts.push(text);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Merge every finding into `issues` (family order), skipping findings at a
    /// file:line the existing issues already cover.
    pub fn merge_into(self, issues: Vec<ReviewIssue>) -> Vec<ReviewIssue> {
        let mut merged = issues;
        for family in [
            self.rules,
            self.secrets,
            self.security,
            self.index_unused,
            self.index_dead,
            self.index_breaking,
        ] {
            if !family.is_empty() {
                merged = rules::merge_rule_findings(merged, family);
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::diff_parser::parse_diff;

    const ROOT: &str = "/fixture/proj";

    /// In-memory index: an unused import and an uncalled function in
    /// `src/app.js`, plus a caller of `important_api`.
    fn fixture_bridge() -> IndexBridge {
        let root = std::path::Path::new(ROOT);
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        crate::index::schema::run_migrations(&conn).expect("migrations");
        let pid = crate::index::ensure_project(&conn, root).expect("project");
        conn.execute(
            "INSERT INTO edges (source, kind, target, file, line, project_id) \
             VALUES ('src/app.js', 'IMPORTS', 'leftpad', 'src/app.js', 1, ?1)",
            [pid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, signature, language, project_id) \
             VALUES ('orphan_helper', 'function', 'src/app.js', 7, 'function orphan_helper()', 'javascript', ?1)",
            [pid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id) \
             VALUES ('main_caller', 'important_api', 'src/main.js', 3, ?1)",
            [pid],
        )
        .unwrap();
        IndexBridge::from_connection(conn, root).expect("bridge")
    }

    const DIFF: &str = "\
diff --git a/src/app.js b/src/app.js
--- a/src/app.js
+++ b/src/app.js
@@ -1,2 +1,6 @@
 import leftpad from 'leftpad';
+const key = 'AKIAIOSFODNN7EXAMPLE';
+const digest = hashlib.md5(data);
+// TODO revisit
+function orphan_helper() {}
diff --git a/src/api.js b/src/api.js
--- a/src/api.js
+++ b/src/api.js
@@ -1,2 +1 @@
-export function important_api() {}
 export const kept = 1;
";

    #[test]
    fn runs_every_family_without_an_llm() {
        let chunks = parse_diff(DIFF);
        let report = run(&chunks, &Config::default(), &fixture_bridge());

        assert!(!report.secrets.is_empty(), "secrets scanner");
        assert!(!report.security.is_empty(), "security scanner");
        assert!(!report.rules.is_empty(), "rule engine");
        assert_eq!(report.index_unused.len(), 1);
        assert_eq!(report.index_unused[0].rule_id, "index-unused-import");
        assert_eq!(report.index_dead.len(), 1);
        assert!(report.index_dead[0].title.contains("orphan_helper"));
        assert_eq!(report.index_breaking.len(), 1);
        assert!(report.index_breaking[0].title.contains("important_api"));
        assert_eq!(report.index_breaking[0].file, "src/api.js");
        assert!(!report.is_empty());
        assert_eq!(
            report.len(),
            report.rules.len()
                + report.secrets.len()
                + report.security.len()
                + report.index_unused.len()
                + report.index_dead.len()
                + report.index_breaking.len()
        );
    }

    #[test]
    fn context_is_ordered_and_optional() {
        let chunks = parse_diff(DIFF);
        let report = run(&chunks, &Config::default(), &fixture_bridge());

        let ctx = report.context(Some("STATIC")).expect("context");
        assert!(ctx.starts_with("STATIC\n\n"));
        let pos = |needle: &str| {
            ctx.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        assert!(pos("index-unused-import") < pos("index-dead-code"));
        assert!(pos("index-dead-code") < pos("index-breaking-change"));
        assert_eq!(
            ctx,
            [
                "STATIC".to_string(),
                rules::format_rule_context(&report.rules),
                rules::format_rule_context(&report.secrets),
                rules::format_rule_context(&report.security),
                rules::format_rule_context(&report.index_unused),
                rules::format_rule_context(&report.index_dead),
                rules::format_rule_context(&report.index_breaking),
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
        );

        let empty = run(
            &parse_diff(""),
            &Config::default(),
            &IndexBridge::unavailable(),
        );
        assert!(empty.is_empty());
        assert_eq!(empty.context(None), None);
        assert_eq!(
            empty.context(Some("only static")).as_deref(),
            Some("only static")
        );
    }

    #[test]
    fn no_index_degrades_to_pattern_scanners_only() {
        let chunks = parse_diff(DIFF);
        let report = run(&chunks, &Config::default(), &IndexBridge::unavailable());
        assert!(!report.secrets.is_empty());
        assert!(report.index_unused.is_empty());
        assert!(report.index_dead.is_empty());
        assert!(report.index_breaking.is_empty());
    }

    #[test]
    fn config_ignore_files_apply_to_index_scans() {
        let chunks = parse_diff(DIFF);
        let mut config = Config::default();
        config.ignore.files.push("src/**".to_string());
        let report = run(&chunks, &config, &fixture_bridge());
        assert!(report.index_unused.is_empty());
        assert!(report.index_dead.is_empty());
        assert!(report.index_breaking.is_empty());
        // Pattern scanners still see the diff: ignore.files only scopes the index.
        assert!(!report.secrets.is_empty());
    }

    #[test]
    fn merge_into_keeps_issues_first_and_skips_covered_locations() {
        let chunks = parse_diff(DIFF);
        let report = run(&chunks, &Config::default(), &fixture_bridge());
        let total = report.len();
        let first = report.secrets[0].clone();
        let llm = vec![ReviewIssue {
            rule_id: None,
            also_matches: Vec::new(),
            file: first.file.clone(),
            line: Some(first.line),
            severity: crate::engine::Severity::Major,
            issue_type: None,
            title: "Hardcoded secret committed to the repository".into(),
            body: String::new(),
            suggested_fix: None,
        }];
        let merged = report.merge_into(llm);
        assert_eq!(
            merged[0].title,
            "Hardcoded secret committed to the repository"
        );
        assert!(merged.len() < total + 1, "covered location is skipped");
        assert!(merged.len() > 1);
    }
}
