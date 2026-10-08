//! High-confidence credential shapes shared by the secrets scanner and the
//! static security scanner.
//!
//! Both scanners deliberately skip test/fixture/example (and, for the security
//! scanner, doc) paths for their noisy generic rules. Real leaked credentials
//! are often committed in exactly those places though, so in those paths they
//! still run this list. Keeping it in one place guarantees the two scanners
//! agree for the same file (#579, follow-up to #573).
//!
//! "High confidence" means a provider-specific prefix or a structured format
//! with a very low false-positive rate: AWS access key IDs, private-key PEM
//! headers, GitHub tokens, Slack tokens and Stripe *live* secret keys.
//! Everything else stays source-path only because it is noisy in tests:
//! generic `password = "..."`/high-entropy rules, JWTs (routinely fixtures),
//! Stripe `test_` keys and `pk_` publishable keys (meant to be public), and
//! LLM-provider keys (OpenAI/Anthropic/Groq/xAI) and Google API keys, which
//! are not part of the shared list yet.

use regex::Regex;
use std::sync::LazyLock;

/// A provider-specific secret shape.
pub struct HighConfidencePattern {
    /// Rule id used by the secrets scanner (`secrets/...`).
    pub id: &'static str,
    pub name: &'static str,
    pub regex: &'static str,
}

pub static HIGH_CONFIDENCE_PATTERNS: &[HighConfidencePattern] = &[
    HighConfidencePattern {
        id: "secrets/aws-access-key",
        name: "AWS Access Key",
        regex: r"AKIA[0-9A-Z]{16}",
    },
    HighConfidencePattern {
        id: "secrets/private-key",
        name: "Private Key Block",
        regex: r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----",
    },
    HighConfidencePattern {
        id: "secrets/github-token",
        name: "GitHub Token",
        regex: r"gh[pousr]_[A-Za-z0-9]{36,}",
    },
    HighConfidencePattern {
        id: "secrets/stripe-live-key",
        name: "Stripe Live Key",
        regex: r"sk_live_[A-Za-z0-9]{24,}",
    },
    HighConfidencePattern {
        id: "secrets/slack-token",
        name: "Slack Token",
        regex: r"xox[baprs]-[A-Za-z0-9-]{10,}",
    },
];

static COMPILED: LazyLock<Vec<(&'static HighConfidencePattern, Regex)>> = LazyLock::new(|| {
    HIGH_CONFIDENCE_PATTERNS
        .iter()
        .map(|p| (p, Regex::new(p.regex).expect("valid high-confidence regex")))
        .collect()
});

/// Published placeholder values are not real: anything containing `EXAMPLE`
/// (e.g. `AKIAIOSFODNN7EXAMPLE`) or ending in a long run of `x` filler
/// (e.g. `ghp_xxxxxxxx...`).
pub fn is_placeholder(matched: &str) -> bool {
    matched.to_uppercase().contains("EXAMPLE")
        || (matched.len() >= 12 && matched.chars().rev().take(12).all(|c| c == 'x' || c == 'X'))
}

/// First non-placeholder high-confidence match in `line`, with its pattern.
pub fn find_high_confidence(line: &str) -> Option<(&'static HighConfidencePattern, &str)> {
    COMPILED.iter().find_map(|(p, re)| {
        re.find_iter(line)
            .map(|m| m.as_str())
            .find(|m| !is_placeholder(m))
            .map(|m| (*p, m))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_shape_and_ignores_placeholders() {
        let aws = format!("{}{}", "AKIA", "QWERTYUIOPASDFGH");
        let gh = format!("{}{}", "ghp_", "a".repeat(36));
        let stripe = format!("{}{}", "sk_live_", "b".repeat(24));
        let slack = format!("{}{}", "xoxb-", "1234567890-abc");
        let pem = format!("-----BEGIN {} KEY-----", "RSA PRIVATE");
        for s in [&aws, &gh, &stripe, &slack, &pem] {
            assert!(find_high_confidence(s).is_some(), "{s}");
        }
        assert!(find_high_confidence("AKIAIOSFODNN7EXAMPLE").is_none());
        let filler = format!("{}{}", "ghp_", "x".repeat(36));
        assert!(find_high_confidence(&filler).is_none());
        assert!(find_high_confidence("password = \"hunter2\"").is_none());
        // A placeholder followed by a real key on one line still reports.
        let mixed = format!("AKIAIOSFODNN7EXAMPLE {aws}");
        assert_eq!(find_high_confidence(&mixed).unwrap().1, aws);
    }
}
