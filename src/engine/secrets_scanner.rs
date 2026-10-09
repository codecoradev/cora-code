/// Deterministic secrets scanner — regex-based pre-scan that detects known secret patterns
/// before the AI review pass. Zero false negatives for well-defined patterns.
use regex::Regex;
use std::sync::LazyLock;
use tracing::debug;

use crate::engine::Severity;
use crate::engine::diff_parser::FileChunk;
use crate::engine::rules::types::RuleFinding;
use crate::engine::scan_input::{self, ScanFile};

// ─── Built-in secret patterns ───

struct SecretPattern {
    id: &'static str,
    name: &'static str,
    regex: &'static str,
    severity: Severity,
}

static PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        id: "secrets/aws-access-key",
        name: "AWS Access Key",
        regex: r"AKIA[0-9A-Z]{16}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/aws-secret-key",
        name: "AWS Secret Key",
        regex: r#"(?i)(?:aws_secret_access_key|aws_secret)\s*[=:]\s*['"][A-Za-z0-9/+=]{40}['"]"#,
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/github-token",
        name: "GitHub Token",
        regex: r"gh[pousr]_[A-Za-z0-9_]{36,255}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/openai-key",
        name: "OpenAI API Key",
        regex: r"sk-[A-Za-z0-9]{20}T3BlbkFJ|sk-proj-[A-Za-z0-9_-]{40,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/anthropic-key",
        name: "Anthropic API Key",
        regex: r"sk-ant-[A-Za-z0-9_-]{20,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/groq-key",
        name: "Groq API Key",
        regex: r"gsk_[A-Za-z0-9]{40,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/private-key",
        name: "Private Key Block",
        regex: r"-----BEGIN\s+(?:RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE KEY-----",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/jwt-token",
        name: "JWT Token",
        regex: r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
        severity: Severity::Major,
    },
    SecretPattern {
        id: "secrets/xai-key",
        name: "xAI API Key",
        regex: r"xai-[A-Za-z0-9_-]{20,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/slack-token",
        name: "Slack Token",
        regex: r"xox[bpras]-[A-Za-z0-9-]{10,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/stripe-key",
        name: "Stripe Key",
        regex: r"(?:sk|pk)_(?:test_|live_)[A-Za-z0-9]{24,}",
        severity: Severity::Critical,
    },
    SecretPattern {
        id: "secrets/google-api-key",
        name: "Google API Key",
        regex: r"AIza[A-Za-z0-9_-]{35}",
        severity: Severity::Major,
    },
];

static COMPILED: LazyLock<Vec<(Regex, &'static SecretPattern)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .filter_map(|p| Regex::new(p.regex).ok().map(|r| (r, p)))
        .collect()
});

// ─── Test fixture patterns to ignore ───

static TEST_FIXTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|/)(?:tests?|specs?|fixtures?|mocks?|examples?)[/_\\.-]|(?:^|/)(?:tests?|specs?)_.*\.").unwrap()
});

// ─── Public API ───

/// Scan the added lines of diff chunks for known secret patterns (diff adapter
/// over [`scan_secrets_in`]).
///
/// Returns findings capped at `max_findings`. Secret values are masked in the
/// message body.
pub fn scan_secrets(chunks: &[FileChunk], max_findings: usize) -> Vec<RuleFinding> {
    scan_secrets_in(&scan_input::from_chunks(chunks), max_findings)
}

/// Scan file lines for known secret patterns. The one implementation behind
/// both the diff and the whole-file entry points.
pub fn scan_secrets_in(files: &[ScanFile<'_>], max_findings: usize) -> Vec<RuleFinding> {
    let mut findings = Vec::new();

    for file in files {
        let file_path = file.path;

        // Test/spec/fixture/mock/example paths skip the generic rules (noisy
        // there) but still get the shared high-confidence list, since real
        // credentials are often committed in exactly those places (#579).
        let is_fixture = TEST_FIXTURE_RE.is_match(file_path);

        for &(line_no, content) in &file.lines {
            if is_fixture {
                if let Some((pat, matched)) =
                    crate::engine::secret_patterns::find_high_confidence(content)
                {
                    findings.push(RuleFinding {
                        rule_id: pat.id.to_string(),
                        file: file_path.to_string(),
                        line: line_no,
                        severity: Severity::Critical,
                        title: format!("[{}] {}", pat.id, pat.name),
                        body: format!(
                            "{} detected in a test/fixture path — verify it is a fake, \
                             otherwise mask with environment variable. Matched: {}",
                            pat.name,
                            mask_secret(matched)
                        ),
                    });
                    if findings.len() >= max_findings {
                        break;
                    }
                }
                continue;
            }

            for (re, pat) in COMPILED.iter() {
                if let Some(m) = re.find(content) {
                    let matched = m.as_str();
                    findings.push(RuleFinding {
                        rule_id: pat.id.to_string(),
                        file: file_path.to_string(),
                        line: line_no,
                        severity: pat.severity,
                        title: format!("[{}] {}", pat.id, pat.name),
                        body: format!(
                            "{} detected — mask with environment variable. Matched: {}",
                            pat.name,
                            mask_secret(matched)
                        ),
                    });
                    if findings.len() >= max_findings {
                        break;
                    }
                }
            }
            if findings.len() >= max_findings {
                break;
            }
        }
        if findings.len() >= max_findings {
            break;
        }
    }

    // Sort Critical first
    findings.sort_by_key(|a| a.severity);
    findings.truncate(max_findings);

    debug!(findings = findings.len(), "secrets pre-scan complete");
    findings
}

/// Mask a secret value: show first 4 and last 4 chars, replace middle with ****.
fn mask_secret(s: &str) -> String {
    if s.len() <= 12 {
        let end = floor_boundary(s, s.len().min(4));
        return format!("{}****", &s[..end]);
    }
    let head = floor_boundary(s, 4);
    // For the tail, count back from the end until we have a valid boundary.
    let tail_start = {
        let mut idx = s.len() - 4;
        while !s.is_char_boundary(idx) {
            idx += 1;
        }
        idx
    };
    format!("{}****{}", &s[..head], &s[tail_start..])
}

/// Find the largest byte index <= `target` that is a valid UTF-8 char boundary.
fn floor_boundary(s: &str, target: usize) -> usize {
    let mut end = target.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::diff_parser::{DiffHunk, DiffLine, DiffLineType};

    fn make_chunk(file: &str, added_lines: &[&str]) -> FileChunk {
        FileChunk {
            old_path: None,
            new_path: Some(file.to_string()),
            language: "py".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 0,
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
                        new_line_no: Some((i + 1) as u32),
                    })
                    .collect(),
            }],
            is_binary: false,
            is_deleted: false,
            is_new: true,
        }
    }

    #[test]
    fn detect_aws_access_key() {
        let chunks = [make_chunk("config.py", &["key = 'AKIAIOSFODNN7EXAMPLE'"])];
        let findings = scan_secrets(&chunks, 10);
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "secrets/aws-access-key")
        );
    }

    #[test]
    fn detect_github_token() {
        let chunks = [make_chunk(
            "config.py",
            &["token = 'ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.iter().any(|f| f.rule_id == "secrets/github-token"));
    }

    #[test]
    fn detect_openai_key() {
        let chunks = [make_chunk(
            "app.py",
            &["api_key = 'sk-proj-abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGHIJ'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.iter().any(|f| f.rule_id == "secrets/openai-key"));
    }

    #[test]
    fn detect_anthropic_key() {
        let chunks = [make_chunk(
            "app.py",
            &["key = 'sk-ant-api03-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "secrets/anthropic-key")
        );
    }

    #[test]
    fn detect_private_key_block() {
        let chunks = [make_chunk(
            "deploy.sh",
            &["echo '-----BEGIN RSA PRIVATE KEY-----'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.iter().any(|f| f.rule_id == "secrets/private-key"));
    }

    #[test]
    fn detect_jwt_token() {
        let chunks = [make_chunk(
            "auth.py",
            &[
                "token = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c'",
            ],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.iter().any(|f| f.rule_id == "secrets/jwt-token"));
    }

    #[test]
    fn detect_groq_key() {
        let chunks = [make_chunk(
            "config.py",
            &["key = 'gsk_abcdefghijklmnopqrstuvwxyz1234567890ABCDEFGH'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.iter().any(|f| f.rule_id == "secrets/groq-key"));
    }

    #[test]
    fn detect_stripe_key_regex() {
        // Verify the Stripe regex pattern compiles and matches expected format
        let re = Regex::new(r"(?:sk|pk)_(?:test_|live_)[A-Za-z0-9]{24,}").unwrap();
        // Build a match string programmatically to avoid push protection
        let prefix = "sk_live_";
        let suffix = "A".repeat(30);
        assert!(re.is_match(&format!("key = '{prefix}{suffix}'")));
        let prefix2 = "pk_test_";
        assert!(re.is_match(&format!("key = '{prefix2}{suffix}'")));
    }

    #[test]
    fn detect_google_api_key() {
        let chunks = [make_chunk(
            "map.py",
            &["key = 'AIzaSyA1234567890abcdefghijklmnopqrstuvwxyz'"],
        )];
        let findings = scan_secrets(&chunks, 10);
        assert!(
            findings
                .iter()
                .any(|f| f.rule_id == "secrets/google-api-key")
        );
    }

    #[test]
    fn detect_slack_token_regex() {
        // Verify the Slack regex pattern compiles and matches expected format
        let re = Regex::new(r"xox[bpras]-[A-Za-z0-9-]{10,}").unwrap();
        // Build match strings programmatically to avoid push protection
        let prefix = "xoxb-";
        let suffix = "A".repeat(40);
        assert!(re.is_match(&format!("token = '{prefix}{suffix}'")));
        let prefix2 = "xoxp-";
        assert!(re.is_match(&format!("token = '{prefix2}{suffix}'")));
    }

    fn fake_aws() -> String {
        format!("{}{}", "AKIA", "QWERTYUIOPASDFGH")
    }

    fn fake_github() -> String {
        format!("{}{}", "ghp_", "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7cF0hJ3kL6")
    }

    fn fake_pem() -> String {
        format!("-----BEGIN {} KEY-----", "RSA PRIVATE")
    }

    #[test]
    fn test_paths_report_high_confidence_secrets() {
        let aws = format!("key = '{}'", fake_aws());
        let gh = format!("token = '{}'", fake_github());
        let pem = fake_pem();
        for (path, line, rule) in [
            ("tests/setup.py", &aws, "secrets/aws-access-key"),
            ("examples/config.py", &gh, "secrets/github-token"),
            ("spec/keys.rb", &pem, "secrets/private-key"),
            ("fixtures/data.py", &aws, "secrets/aws-access-key"),
            ("test_config.py", &gh, "secrets/github-token"),
            ("pkg/client_test.go", &aws, "secrets/aws-access-key"),
        ] {
            let findings = scan_secrets(&[make_chunk(path, &[line.as_str()])], 10);
            assert_eq!(findings.len(), 1, "{path}");
            assert_eq!(findings[0].rule_id, rule, "{path}");
            assert!(!findings[0].body.contains(&fake_aws()), "masked");
        }
    }

    #[test]
    fn test_paths_ignore_placeholders() {
        let filler = format!("token = '{}{}'", "ghp_", "x".repeat(36));
        for path in ["tests/a.py", "examples/b.py", "test_c.py"] {
            let chunks = [make_chunk(
                path,
                &["key = 'AKIAIOSFODNN7EXAMPLE'", filler.as_str()],
            )];
            assert!(scan_secrets(&chunks, 10).is_empty(), "{path}");
        }
    }

    #[test]
    fn test_paths_keep_generic_rules_suppressed() {
        let jwt = format!(
            "t = '{}.{}.{}'",
            "eyJhbGciOiJIUzI1NiJ9", "eyJzdWIiOiIxMjM0NTY3ODkw", "abcdefghijkl"
        );
        let stripe_test = format!("k = '{}{}'", "sk_test_", "a".repeat(24));
        let chunks = [make_chunk(
            "tests/auth.py",
            &["password = \"hunter2\"", jwt.as_str(), stripe_test.as_str()],
        )];
        assert!(scan_secrets(&chunks, 10).is_empty());
    }

    #[test]
    fn source_paths_report_everything_as_before() {
        let gh = format!("token = '{}'", fake_github());
        let stripe_test = format!("k = '{}{}'", "sk_test_", "a".repeat(24));
        let findings = scan_secrets(
            &[make_chunk(
                "src/app.py",
                &[gh.as_str(), stripe_test.as_str()],
            )],
            10,
        );
        let ids: Vec<_> = findings.iter().map(|f| f.rule_id.as_str()).collect();
        assert!(ids.contains(&"secrets/github-token"));
        assert!(ids.contains(&"secrets/stripe-key"));
    }

    #[test]
    fn scanners_agree_on_shared_patterns() {
        use crate::engine::secret_patterns::HIGH_CONFIDENCE_PATTERNS;
        use crate::engine::security_scanner::scan_security;
        let samples = [
            fake_aws(),
            fake_pem(),
            fake_github(),
            format!("{}{}", "sk_live_", "b".repeat(24)),
            format!("{}{}", "xoxb-", "1234567890-abcdef"),
        ];
        assert_eq!(samples.len(), HIGH_CONFIDENCE_PATTERNS.len());
        for path in ["tests/x.rs", "examples/y.py"] {
            for sample in &samples {
                let line = format!("v = {sample}");
                let a = scan_secrets(&[make_chunk(path, &[line.as_str()])], 10);
                let b = scan_security(&[make_chunk(path, &[line.as_str()])], 10);
                assert_eq!(a.len(), 1, "secrets_scanner {path} {sample}");
                assert_eq!(b.len(), 1, "security_scanner {path} {sample}");
            }
            for ph in ["AKIAIOSFODNN7EXAMPLE", "password = \"hunter2\""] {
                let a = scan_secrets(&[make_chunk(path, &[ph])], 10);
                let b = scan_security(&[make_chunk(path, &[ph])], 10);
                assert_eq!(a.is_empty(), b.is_empty(), "{path} {ph}");
                assert!(a.is_empty());
            }
        }
    }

    #[test]
    fn max_findings_cap() {
        let chunks = [make_chunk(
            "config.py",
            &[
                "a = 'AKIAIOSFODNN7EXAMPL1'",
                "b = 'AKIAIOSFODNN7EXAMPL2'",
                "c = 'AKIAIOSFODNN7EXAMPL3'",
                "d = 'AKIAIOSFODNN7EXAMPL4'",
                "e = 'AKIAIOSFODNN7EXAMPL5'",
            ],
        )];
        let findings = scan_secrets(&chunks, 3);
        assert_eq!(findings.len(), 3);
    }

    #[test]
    fn mask_secret_short() {
        assert_eq!(mask_secret("AKIA1234"), "AKIA****");
    }

    #[test]
    fn mask_secret_long() {
        assert_eq!(
            mask_secret("sk-proj-abcdefghijklmnopqrstuvwxyz1234567890"),
            "sk-p****7890"
        );
    }

    #[test]
    fn mask_secret_medium() {
        assert_eq!(mask_secret("ghp_abcdef"), "ghp_****");
    }

    #[test]
    fn mask_secret_multibyte_no_panic() {
        // Secret containing multi-byte UTF-8 characters.
        // Without char-boundary checking, &s[..4] could split a codepoint.
        let secret = "🔒secret-api-key-value-1234567890";
        // Should not panic and should still mask the middle.
        let masked = mask_secret(secret);
        assert!(masked.contains("****"));
    }

    #[test]
    fn mask_secret_multibyte_short_no_panic() {
        // Short secret (≤12 bytes) with multi-byte chars.
        // "🔒ab" = 4 + 1 + 1 = 6 bytes, 3 chars.
        let secret = "🔒ab";
        let masked = mask_secret(secret);
        assert!(masked.ends_with("****"));
    }

    #[test]
    fn no_secrets_clean_code() {
        let chunks = [make_chunk("main.py", &["x = 42", "print('hello')"])];
        let findings = scan_secrets(&chunks, 10);
        assert!(findings.is_empty());
    }

    #[test]
    fn only_added_lines_scanned() {
        // Build a chunk with a removed line containing a secret
        let chunk = FileChunk {
            old_path: None,
            new_path: Some("config.py".to_string()),
            language: "py".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Remove,
                        content: "key = 'AKIAIOSFODNN7EXAMPLE'".to_string(),
                        old_line_no: Some(1),
                        new_line_no: None,
                    },
                    DiffLine {
                        line_type: DiffLineType::Add,
                        content: "key = env('AWS_KEY')".to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        };
        let findings = scan_secrets(&[chunk], 10);
        assert!(findings.is_empty(), "removed lines should not be flagged");
    }
}
