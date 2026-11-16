//! Reflection pass: a cheap deterministic second look at each finding before
//! it is kept (#723).
//!
//! The judge is wrong in a few recurring, recognisable ways: a security or
//! correctness claim about prose in a documentation file (a CORS wildcard in a
//! README), a finding in a lockfile, and `unwrap`/`panic`/`println` flagged in
//! test code. Each is cheap to recognise from the finding itself, so the check
//! costs no tokens: it drops or steps down the finding before it is stored.
//!
//! The rules are deliberately narrow. When the finding does not clearly match
//! one, it is kept as the judge reported it.

use crate::engine::test_context::is_test_path;
use crate::engine::types::{ReviewIssue, Severity};

/// What the reflection pass decided for one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Report it one severity level lower.
    StepDown,
    Drop,
}

const LOCKFILES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    "uv.lock",
];

/// Categories that make no sense for prose: they describe running code.
const CODE_ONLY_TYPES: &[&str] = &[
    "security",
    "vulnerability",
    "bug",
    "bugs",
    "performance",
    "reliability",
    "concurrency",
    "correctness",
];

/// A finding about prose that is about a leaked credential is kept.
const LEAK_WORDS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "token",
    "api key",
    "api_key",
    "apikey",
    "credential",
    "private key",
];

/// Idioms that are fine, even idiomatic, in a test.
const TEST_OK_WORDS: &[&str] = &["unwrap", "expect(", "panic", "println", "dbg!"];

/// Names of plain-text files that are documentation. Any other `.txt` file may
/// be a build or dependency manifest (`CMakeLists.txt`, `requirements.txt`,
/// `constraints.txt`), where a real finding must not be dropped.
const TXT_DOC_NAMES: &[&str] = &[
    "readme",
    "changelog",
    "changes",
    "history",
    "news",
    "license",
    "licence",
    "copying",
    "notice",
    "authors",
    "contributors",
    "contributing",
    "install",
    "todo",
    "notes",
];

/// Prose and markup files, where a claim about running code has nothing to
/// point at. Markdown, reStructuredText, AsciiDoc, TeX and Org are always
/// prose; `.txt` only when it is named like a document or sits in a docs
/// directory, so manifests such as `requirements.txt` stay reviewed.
fn is_prose_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let Some(dot) = lower.rfind('.') else {
        return false;
    };
    match &lower[dot + 1..] {
        "md" | "markdown" | "mdx" | "rst" | "adoc" | "asciidoc" | "tex" | "org" => true,
        "txt" => {
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let stem = name.strip_suffix(".txt").unwrap_or(name);
            let in_docs = lower.split('/').any(|seg| matches!(seg, "doc" | "docs"));
            TXT_DOC_NAMES.contains(&stem) || in_docs
        }
        _ => false,
    }
}

fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    LOCKFILES.contains(&name.as_str()) || name.ends_with(".lock")
}

/// Judge one finding.
pub fn reflect(issue: &ReviewIssue) -> Verdict {
    if is_lockfile(&issue.file) {
        return Verdict::Drop;
    }
    let kind = issue
        .issue_type
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if is_prose_file(&issue.file) && CODE_ONLY_TYPES.contains(&kind.as_str()) {
        // A leaked credential in a README is real, whatever the file type.
        let text = format!("{} {}", issue.title, issue.body).to_ascii_lowercase();
        if !LEAK_WORDS.iter().any(|w| text.contains(w)) {
            return Verdict::Drop;
        }
    }
    if is_test_path(&issue.file) {
        let text = format!("{} {}", issue.title, issue.body).to_ascii_lowercase();
        if TEST_OK_WORDS.iter().any(|w| text.contains(w)) {
            return Verdict::StepDown;
        }
    }
    Verdict::Keep
}

/// One level down; `info` stays `info`.
fn step_down(s: Severity) -> Severity {
    match s {
        Severity::Critical => Severity::Major,
        Severity::Major => Severity::Minor,
        Severity::Minor | Severity::Info => Severity::Info,
    }
}

/// Apply the pass: dropped findings are removed, stepped-down ones lowered.
pub fn apply(issues: Vec<ReviewIssue>) -> Vec<ReviewIssue> {
    issues
        .into_iter()
        .filter_map(|mut i| match reflect(&i) {
            Verdict::Keep => Some(i),
            Verdict::StepDown => {
                tracing::debug!(file = %i.file, title = %i.title, "reflection: stepped down");
                i.severity = step_down(i.severity);
                Some(i)
            }
            Verdict::Drop => {
                tracing::debug!(file = %i.file, title = %i.title, "reflection: dropped");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(file: &str, kind: &str, sev: Severity, text: &str) -> ReviewIssue {
        ReviewIssue {
            file: file.to_string(),
            line: Some(3),
            severity: sev,
            issue_type: Some(kind.to_string()),
            title: text.to_string(),
            body: text.to_string(),
            suggested_fix: None,
            rule_id: None,
            also_matches: Vec::new(),
            snippet: None,
        }
    }

    #[test]
    fn a_security_claim_about_a_markdown_file_is_dropped() {
        let i = issue(
            "docs/api.md",
            "security",
            Severity::Major,
            "CORS wildcard origin",
        );
        assert_eq!(reflect(&i), Verdict::Drop);
    }

    #[test]
    fn a_leaked_credential_in_a_markdown_file_is_kept() {
        let i = issue(
            "README.md",
            "security",
            Severity::Critical,
            "API key committed in the setup section",
        );
        assert_eq!(reflect(&i), Verdict::Keep);
    }

    /// Build and dependency manifests are `.txt` but not prose: a real
    /// finding there is kept.
    #[test]
    fn txt_manifests_are_not_treated_as_prose() {
        for f in [
            "CMakeLists.txt",
            "requirements.txt",
            "backend/requirements-dev.txt",
            "constraints.txt",
            "sub/CMakeLists.txt",
        ] {
            let i = issue(f, "security", Severity::Major, "unpinned dependency");
            assert_eq!(reflect(&i), Verdict::Keep, "{f}");
        }
    }

    #[test]
    fn txt_documents_are_still_prose() {
        for f in ["README.txt", "docs/notes-on-cors.txt", "CHANGELOG.txt"] {
            let i = issue(f, "security", Severity::Major, "CORS wildcard origin");
            assert_eq!(reflect(&i), Verdict::Drop, "{f}");
        }
    }

    #[test]
    fn a_style_note_on_a_markdown_file_is_kept() {
        let i = issue(
            "README.md",
            "style",
            Severity::Info,
            "heading level skipped",
        );
        assert_eq!(reflect(&i), Verdict::Keep);
    }

    #[test]
    fn a_finding_in_a_lockfile_is_dropped() {
        for f in ["Cargo.lock", "web/package-lock.json", "deps.lock"] {
            assert_eq!(
                reflect(&issue(f, "bugs", Severity::Minor, "x")),
                Verdict::Drop
            );
        }
    }

    #[test]
    fn unwrap_in_a_test_file_is_stepped_down() {
        let i = issue("tests/api.rs", "bugs", Severity::Major, "unwrap may panic");
        assert_eq!(reflect(&i), Verdict::StepDown);
        let out = apply(vec![i]);
        assert_eq!(out[0].severity, Severity::Minor);
    }

    #[test]
    fn unwrap_in_production_code_is_kept_as_reported() {
        let i = issue("src/api.rs", "bugs", Severity::Major, "unwrap may panic");
        assert_eq!(reflect(&i), Verdict::Keep);
    }

    #[test]
    fn a_real_bug_in_a_test_file_is_kept() {
        let i = issue(
            "tests/api.rs",
            "bugs",
            Severity::Major,
            "assertion compares wrong field",
        );
        assert_eq!(reflect(&i), Verdict::Keep);
    }

    #[test]
    fn info_does_not_step_below_info() {
        assert_eq!(step_down(Severity::Info), Severity::Info);
        assert_eq!(step_down(Severity::Critical), Severity::Major);
    }
}
