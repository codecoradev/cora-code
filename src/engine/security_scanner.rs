//! Static security scanner — deterministic pattern detection for common vulnerabilities.
//!
//! Runs before LLM review. Catches what's guaranteed to be a finding:
//! weak crypto, SQL injection, code injection, auth issues, debug config.
//! Secrets are handled separately by `secrets_scanner.rs`.

use regex::Regex;
use std::sync::LazyLock;
use tracing::debug;

use crate::engine::Severity;
use crate::engine::diff_parser::{DiffLineType, FileChunk};
use crate::engine::rules::builtin::post_match_filter;
use crate::engine::rules::types::RuleFinding;

// ─── Security patterns ───

pub struct SecurityPattern {
    pub id: &'static str,
    pub name: &'static str,
    pub regex: &'static str,
    pub severity: Severity,
}

pub static PATTERNS: &[SecurityPattern] = &[
    // ── Weak crypto ──
    SecurityPattern {
        id: "crypto/md5-password",
        name: "MD5 used for password hashing",
        regex: r"(?i)md5\s*\(\s*(?:password|passwd|pwd|secret)",
        severity: Severity::Critical,
    },
    SecurityPattern {
        id: "crypto/sha1-password",
        name: "SHA-1 used for password hashing",
        regex: r"(?i)sha1\s*\(\s*(?:password|passwd|pwd|secret)",
        severity: Severity::Major,
    },
    SecurityPattern {
        id: "crypto/weak-hash",
        name: "Weak hash algorithm (MD5/SHA1) for security-sensitive data",
        regex: r"(?i)(?:hashlib\.md5|hashlib\.sha1|MD5Create|SHA1Create|Digest::MD5|Digest::SHA1)",
        severity: Severity::Major,
    },
    SecurityPattern {
        id: "crypto/hardcoded-secret",
        name: "Hardcoded password or secret in variable",
        regex: r"(?i)(?:password|passwd|pwd|secret|api_key|apikey|token)\s*[=:]\s*\S{8,}",
        severity: Severity::Critical,
    },
    // ── Injection ──
    SecurityPattern {
        id: "injection/sql-concat",
        name: "SQL injection via string concatenation",
        regex: r"(?i)(?:SELECT|INSERT|UPDATE|DELETE)\s+.*\+",
        severity: Severity::Critical,
    },
    SecurityPattern {
        id: "injection/eval",
        name: "eval() with dynamic input",
        regex: r"(?i)eval\s*\(\s*(?:req|request|input|params|data|user)",
        severity: Severity::Critical,
    },
    SecurityPattern {
        id: "injection/exec",
        name: "Command injection via exec/system with dynamic input",
        // Only flag when there's a dynamic input signal on the same line
        // (f-string, format(), string concat with +, shell=True, or raw user input).
        // subprocess.run(["cmd", "arg"]) with literal list is safe and should not trigger.
        regex: r#"(?i)(?:exec|system|popen|subprocess\.(?:call|run))\s*\((?:[^)]*(?:f"|f'|format\(|\.format\(|shell\s*=\s*True|\+\s*(?:req|request|input|params|data|user|query)|%s|%\(.*\)))"#,
        severity: Severity::Critical,
    },
    // ── Auth issues ──
    SecurityPattern {
        id: "auth/hardcoded-role",
        name: "Hardcoded role or permission check",
        regex: r"(?i)role\s*==\s*(?:admin|super|root)|is_admin\s*==\s*True",
        severity: Severity::Major,
    },
    // ── Debug config ──
    SecurityPattern {
        id: "config/debug-enabled",
        name: "Debug mode enabled (production risk)",
        regex: r"(?i)(?:DEBUG\s*=\s*True|debug:\s*true|--debug)",
        severity: Severity::Minor,
    },
    SecurityPattern {
        id: "config/cors-wildcard",
        name: "CORS wildcard allows all origins",
        // Match actual code patterns, not the word "cors" in prose/documentation.
        // Require either the literal HTTP header with `*`, or a code assignment/call
        // like `cors = "*"`, `origin: *`, `allowed_origins = "*"`, `allow_origin("*")`.
        // The word "cors" alone is too broad — it appears in env var names
        // (TITEN_CORS_ORIGINS), config keys, and documentation.
        regex: r#"(?i)(?:Access-Control-Allow-Origin\s*:\s*\*|(?:cors|allow_origin|allowed_origins)\s*[=:(]\s*["']?\*["']?|origin\s*[=:]\s*["']?\*["']?)"#,
        severity: Severity::Major,
    },
    // ── TLS/SSL ──
    SecurityPattern {
        id: "crypto/ssl-verify-disabled",
        name: "SSL certificate verification disabled",
        regex: r"(?i)(?:verify\s*=\s*False|verify:\s*false|rejectUnauthorized:\s*false)",
        severity: Severity::Critical,
    },
];

static COMPILED_PATTERNS: LazyLock<Vec<(String, String, Regex, Severity)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .filter_map(|p| {
            Regex::new(p.regex)
                .ok()
                .map(|re| (p.id.to_string(), p.name.to_string(), re, p.severity))
        })
        .collect()
});

/// Run static security scan on diff chunks.
///
/// Only scans added lines (not removed/context) to reduce false positives.
/// Returns findings sorted by severity (worst first).
pub fn scan_security(chunks: &[FileChunk], max_findings: usize) -> Vec<RuleFinding> {
    let mut findings = Vec::new();

    for chunk in chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("unknown");

        // Skip test/spec/fixture/mock/example files
        if is_test_file(path) {
            debug!(file = path, "skipping test file in security scan");
            continue;
        }

        // Skip documentation and non-code files — security patterns are designed
        // for source code, not prose. Prevents false positives like flagging
        // "CORS" in a markdown config guide (#483).
        if is_doc_file(path) {
            debug!(file = path, "skipping documentation file in security scan");
            continue;
        }

        for hunk in &chunk.chunks {
            for line in &hunk.lines {
                if line.line_type != DiffLineType::Add {
                    continue;
                }

                let line_no = line.new_line_no.unwrap_or(0);
                if line_no == 0 {
                    continue;
                }

                for (rule_id, name, regex, severity) in COMPILED_PATTERNS.iter() {
                    if regex.is_match(&line.content) {
                        // Apply post-match filter to suppress known false positives
                        if post_match_filter(rule_id, &line.content) {
                            debug!(
                                rule = %rule_id,
                                file = path,
                                line = line_no,
                                "security pattern suppressed by post-match filter"
                            );
                            continue;
                        }

                        debug!(
                            rule = %rule_id,
                            file = path,
                            line = line_no,
                            "security pattern match"
                        );
                        findings.push(RuleFinding {
                            rule_id: rule_id.clone(),
                            file: path.to_string(),
                            line: line_no,
                            severity: *severity,
                            title: name.clone(),
                            body: format!(
                                "Static security scanner detected: {} in {}:{}",
                                name, path, line_no
                            ),
                        });

                        if findings.len() >= max_findings {
                            findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
                            return findings;
                        }
                    }
                }
            }
        }
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
    findings
}

/// Check if a file path looks like a test/spec/fixture/mock/example file.
///
/// Uses path-segment awareness so common words like `latest`, `aspect`,
/// `attestation`, `protest` are not mistaken for test files (#87). A segment
/// is any run between `/`, `_`, `-`, and `.` separators.
fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    for seg in lower.split(['/', '_', '-', '.']) {
        if matches!(
            seg,
            "test"
                | "tests"
                | "testing"
                | "tested"
                | "__tests__"
                | "spec"
                | "specs"
                | "fixture"
                | "fixtures"
                | "mock"
                | "mocks"
                | "example"
                | "examples"
        ) {
            return true;
        }
    }
    false
}

/// Check if a file path is a documentation or non-code file.
///
/// Security scanner patterns are designed for source code. Scanning markdown,
/// plain text, or reStructuredText produces false positives because security
/// keywords (CORS, secret, password) appear naturally in documentation prose.
fn is_doc_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    matches!(
        lower.rsplit('.').next().unwrap_or(""),
        "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc" | "asciidoc" | "tex" | "org"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(file: &str, added_lines: &[&str]) -> FileChunk {
        use crate::engine::diff_parser::{DiffHunk, DiffLine};
        FileChunk {
            old_path: None,
            new_path: Some(file.to_string()),
            language: "py".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: added_lines.len() as u32,
                header: String::new(),
                lines: added_lines
                    .iter()
                    .enumerate()
                    .map(|(i, content)| DiffLine {
                        line_type: DiffLineType::Add,
                        content: content.to_string(),
                        old_line_no: None,
                        new_line_no: Some(i as u32 + 1),
                    })
                    .collect(),
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }
    }

    #[test]
    fn detects_hardcoded_password() {
        let chunks = vec![make_chunk(
            "src/auth.rs",
            &["let password = supersecret123;"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
        assert!(findings[0].rule_id.contains("hardcoded-secret"));
    }

    #[test]
    fn detects_sql_injection() {
        let chunks = vec![make_chunk(
            "src/db.py",
            &["query = \"SELECT * FROM users WHERE id = \" + req.params.id"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
        assert!(findings[0].rule_id.contains("sql"));
    }

    #[test]
    fn detects_eval_injection() {
        let chunks = vec![make_chunk("src/run.js", &["eval(request.body.code)"])];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
        assert!(findings[0].rule_id.contains("eval"));
    }

    #[test]
    fn detects_debug_enabled() {
        let chunks = vec![make_chunk("src/config.py", &["DEBUG = True"])];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
        assert!(findings[0].rule_id.contains("debug"));
    }

    #[test]
    fn detects_ssl_verify_disabled() {
        let chunks = vec![make_chunk(
            "src/client.py",
            &["requests.get(url, verify=False)"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
        assert!(findings[0].rule_id.contains("ssl"));
    }

    #[test]
    fn detects_cors_wildcard() {
        let chunks = vec![make_chunk(
            "src/server.rs",
            &["Access-Control-Allow-Origin: *"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(!findings.is_empty());
    }

    #[test]
    fn skips_test_files() {
        let chunks = vec![make_chunk(
            "tests/auth_test.py",
            &["let password = test_password_123;"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(findings.is_empty());
    }

    #[test]
    fn is_test_file_does_not_over_match_common_words() {
        // #87: substring matching caught false positives like 'latest', 'aspect'.
        assert!(!is_test_file("src/latest_config.rs"));
        assert!(!is_test_file("src/models/attestation.rs"));
        assert!(!is_test_file("src/utils/aspect.rs"));
        assert!(!is_test_file("src/protest.rs"));
        assert!(!is_test_file("src/inspector.rs"));
        // Real test files still match.
        assert!(is_test_file("tests/auth_test.py"));
        assert!(is_test_file("src/app.test.ts"));
        assert!(is_test_file("src/__tests__/setup.rs"));
        assert!(is_test_file("spec/models/user_spec.rb"));
    }

    #[test]
    fn empty_diff_no_findings() {
        let findings = scan_security(&[], 10);
        assert!(findings.is_empty());
    }

    #[test]
    fn respects_max_findings() {
        let chunks = vec![
            make_chunk("src/a.py", &["DEBUG = True", "eval(input.data)"]),
            make_chunk("src/b.py", &["let password = supersecret"]),
        ];
        let findings = scan_security(&chunks, 2);
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn sorts_by_severity() {
        let chunks = vec![make_chunk(
            "src/app.py",
            &["DEBUG = True", "let password = supersecret"],
        )];
        let findings = scan_security(&chunks, 10);
        // Critical (hardcoded-secret) should come before Minor (debug)
        assert!(findings[0].severity >= findings[findings.len() - 1].severity);
    }

    #[test]
    fn subprocess_run_with_literal_list_no_false_positive() {
        let chunks = vec![make_chunk(
            "src/provider.py",
            &["cmd = [self._bin, 'recall', query]", "subprocess.run(cmd)"],
        )];
        let findings = scan_security(&chunks, 10);
        let exec_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "injection/exec")
            .collect();
        assert!(
            exec_findings.is_empty(),
            "subprocess.run(cmd) with controlled list should not trigger"
        );
    }

    #[test]
    fn subprocess_run_with_fstring_triggers() {
        let chunks = vec![make_chunk(
            "src/handler.py",
            &["subprocess.run(f'cat {user_input}')"],
        )];
        let findings = scan_security(&chunks, 10);
        let exec_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "injection/exec")
            .collect();
        assert_eq!(
            exec_findings.len(),
            1,
            "subprocess.run with f-string should trigger"
        );
    }

    #[test]
    fn subprocess_run_with_shell_true_triggers() {
        let chunks = vec![make_chunk(
            "src/handler.py",
            &["subprocess.run(cmd, shell=True)"],
        )];
        let findings = scan_security(&chunks, 10);
        let exec_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "injection/exec")
            .collect();
        assert_eq!(
            exec_findings.len(),
            1,
            "subprocess.run with shell=True should trigger"
        );
    }

    // ─── False positive suppression via post_match_filter (issue #357) ───

    #[test]
    fn svg_xmlns_url_suppressed_in_security_scan() {
        let chunks = vec![make_chunk(
            "src/icon.svg",
            &[r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">"#],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(
            findings.is_empty(),
            "SVG xmlns should not trigger security findings"
        );
    }

    #[test]
    fn empty_secret_value_suppressed_in_security_scan() {
        let chunks = vec![make_chunk(
            "src/Form.svelte",
            &["let formAppSecret = $state('');"],
        )];
        let findings = scan_security(&chunks, 10);
        let secret_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "crypto/hardcoded-secret")
            .collect();
        assert!(
            secret_findings.is_empty(),
            "Empty $state('') secret should not trigger"
        );
    }

    #[test]
    fn real_hardcoded_secret_still_detected_after_filter() {
        let chunks = vec![make_chunk(
            "src/config.py",
            &["API_KEY = \"sk_live_abc123def456ghi789\""],
        )];
        let findings = scan_security(&chunks, 10);
        let secret_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "crypto/hardcoded-secret")
            .collect();
        assert_eq!(
            secret_findings.len(),
            1,
            "Real hardcoded secret should still be detected"
        );
    }

    // ─── CORS false positive tests (issue #483) ───

    #[test]
    fn detects_cors_wildcard_header() {
        // The classic dangerous pattern — actual HTTP header with wildcard
        let chunks = vec![make_chunk(
            "src/server.rs",
            &["Access-Control-Allow-Origin: *"],
        )];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        assert_eq!(cors_findings.len(), 1, "Should detect wildcard CORS header");
    }

    #[test]
    fn detects_cors_wildcard_assignment() {
        // Code assignment like cors = "*" or origin = '*'
        let chunks = vec![make_chunk("src/config.rs", &["let cors_origin = \"*\";"])];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        assert_eq!(
            cors_findings.len(),
            1,
            "Should detect cors assignment with wildcard"
        );
    }

    #[test]
    fn detects_allowed_origins_wildcard() {
        // Pattern: allowed_origins = "*"
        let chunks = vec![make_chunk("src/app.py", &["allowed_origins = \"*\""])];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        assert_eq!(
            cors_findings.len(),
            1,
            "Should detect allowed_origins with wildcard"
        );
    }

    #[test]
    fn no_false_positive_cors_env_var_name() {
        // Issue #483: TITEN_CORS_ORIGINS contains "cors" but is not a wildcard assignment.
        // The * after it comes from markdown bold (**), not a CORS wildcard.
        let chunks = vec![make_chunk(
            "src/config.rs",
            &["let val = std::env::var(\"TITEN_CORS_ORIGINS\").unwrap_or(\"*\");"],
        )];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        assert!(
            cors_findings.is_empty(),
            "TITEN_CORS_ORIGINS env var name should not trigger CORS wildcard"
        );
    }

    #[test]
    fn no_false_positive_cors_in_prose() {
        // Issue #483: "CORS" keyword in documentation prose near a `*` character
        let chunks = vec![make_chunk(
            "src/config.rs",
            &["// CORS configured via TITEN_CORS_ORIGINS env var"],
        )];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        assert!(
            cors_findings.is_empty(),
            "CORS keyword in comment prose should not trigger"
        );
    }

    #[test]
    fn no_false_positive_markdown_file() {
        // Issue #483: .md files should be skipped entirely by security scanner
        let chunks = vec![make_chunk(
            "docs/deployment.md",
            &["Access-Control-Allow-Origin: *"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(
            findings.is_empty(),
            "Markdown files should not be scanned by security scanner"
        );
    }

    #[test]
    fn no_false_positive_txt_file() {
        let chunks = vec![make_chunk(
            "docs/security.txt",
            &["Access-Control-Allow-Origin: *"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(
            findings.is_empty(),
            "Text files should not be scanned by security scanner"
        );
    }

    #[test]
    fn no_false_positive_rst_file() {
        let chunks = vec![make_chunk(
            "docs/api.rst",
            &["Access-Control-Allow-Origin: *"],
        )];
        let findings = scan_security(&chunks, 10);
        assert!(
            findings.is_empty(),
            "reStructuredText files should not be scanned"
        );
    }

    #[test]
    fn real_cors_wildcard_in_rust_still_detected() {
        // Make sure we don't over-suppress — actual code patterns still trigger
        let chunks = vec![make_chunk(
            "src/server.rs",
            &[".layer(CorsLayer::new().allow_origin(\"*\"))"],
        )];
        let findings = scan_security(&chunks, 10);
        let cors_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule_id == "config/cors-wildcard")
            .collect();
        // This should trigger because it's a code assignment pattern: origin = "*"
        assert_eq!(
            cors_findings.len(),
            1,
            "Real CORS wildcard in Rust code should be detected"
        );
    }

    #[test]
    fn is_doc_file_recognizes_common_extensions() {
        assert!(is_doc_file("README.md"));
        assert!(is_doc_file("docs/guide.markdown"));
        assert!(is_doc_file("docs/api.mdx"));
        assert!(is_doc_file("notes.txt"));
        assert!(is_doc_file("docs/spec.rst"));
        assert!(is_doc_file("docs/manual.adoc"));
        assert!(is_doc_file("docs/manual.asciidoc"));
        assert!(is_doc_file("paper.tex"));
        assert!(is_doc_file("notes.org"));
    }

    #[test]
    fn is_doc_file_does_not_match_code_files() {
        assert!(!is_doc_file("src/main.rs"));
        assert!(!is_doc_file("src/app.py"));
        assert!(!is_doc_file("src/server.ts"));
        assert!(!is_doc_file("src/index.js"));
        assert!(!is_doc_file("src/config.go"));
        assert!(!is_doc_file("Dockerfile"));
        assert!(!is_doc_file("docker-compose.yml"));
        assert!(!is_doc_file("Makefile"));
    }
}
