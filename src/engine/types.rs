use serde::{Deserialize, Serialize};
use std::fmt;

/// Issue severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Critical,
    Major,
    Minor,
    Info,
}

impl Severity {
    /// Parse from string (case-insensitive, no allocation — #10)
    pub fn from_str_lossy(s: &str) -> Self {
        if s.eq_ignore_ascii_case("critical") {
            Severity::Critical
        } else if s.eq_ignore_ascii_case("major") {
            Severity::Major
        } else if s.eq_ignore_ascii_case("minor") {
            Severity::Minor
        } else {
            Severity::Info
        }
    }

    /// Get the label text for this severity.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "CRITICAL",
            Severity::Major => "MAJOR",
            Severity::Minor => "MINOR",
            Severity::Info => "INFO",
        }
    }

    /// Get the icon for this severity.
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Critical => "🔴",
            Severity::Major => "🟠",
            Severity::Minor => "🟡",
            Severity::Info => "ℹ️",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Critical => write!(f, "critical"),
            Severity::Major => write!(f, "major"),
            Severity::Minor => write!(f, "minor"),
            Severity::Info => write!(f, "info"),
        }
    }
}

/// Issue type categories.
///
/// Kept for API completeness — used for deserialization and future typed
/// issue-type matching. Currently the LLM returns `issue_type` as a string,
/// but this enum provides a structured alternative.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Security,
    Performance,
    Bug,
    BestPractice,
    Style,
    Suggestion,
}

#[allow(dead_code)]
impl fmt::Display for IssueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IssueType::Security => write!(f, "security"),
            IssueType::Performance => write!(f, "performance"),
            IssueType::Bug => write!(f, "bug"),
            IssueType::BestPractice => write!(f, "best_practice"),
            IssueType::Style => write!(f, "style"),
            IssueType::Suggestion => write!(f, "suggestion"),
        }
    }
}

#[allow(dead_code)]
impl IssueType {
    /// Parse from string with lenient matching (accepts plural forms, etc.)
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "security" | "sec" => IssueType::Security,
            "performance" | "perf" => IssueType::Performance,
            "bug" | "bugs" => IssueType::Bug,
            "best_practice" | "best-practice" | "bestpractice" | "best practice" => {
                IssueType::BestPractice
            }
            "style" | "formatting" => IssueType::Style,
            // suggestion/info merged into wildcard — all three arms returned
            // identical IssueType::Suggestion, verified by clippy match_same_arms
            _ => IssueType::Suggestion,
        }
    }
}

/// A single review issue found in code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub severity: Severity,
    /// Issue type/category — stored as string since LLM output varies.
    /// Common values: security, performance, bug, `best_practice`, style, suggestion
    #[serde(alias = "type", alias = "issue_type")]
    pub issue_type: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub suggested_fix: Option<String>,
    /// Id of the deterministic rule/scanner that produced this finding (e.g.
    /// `sec-hardcoded-secret`); `None` for LLM findings. Lets `cora-ignore:` and
    /// `ignore.rules` match by id as well as title (#597).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// Ids/titles of scanner findings dropped in merge because this issue sits
    /// on the same line. Suppression honors them, so a marker naming the
    /// scanner rule still hides the surviving issue. Never serialized.
    #[serde(skip)]
    pub also_matches: Vec<String>,
}

impl ReviewIssue {
    /// Minimal issue: no type, empty body, no fix, no rule id. Chain the
    /// `with_*` setters for the rest. Construction sites go through this so a
    /// new field is added in one place (#610).
    pub fn new(
        file: impl Into<String>,
        line: Option<u32>,
        severity: Severity,
        title: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            line,
            severity,
            issue_type: None,
            title: title.into(),
            body: String::new(),
            suggested_fix: None,
            rule_id: None,
            also_matches: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_type(mut self, issue_type: impl Into<String>) -> Self {
        self.issue_type = Some(issue_type.into());
        self
    }

    #[must_use]
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.suggested_fix = Some(fix.into());
        self
    }

    #[must_use]
    pub fn with_rule_id(mut self, id: impl Into<String>) -> Self {
        self.rule_id = Some(id.into());
        self
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    // ─── Severity::from_str_lossy ───

    #[test]
    fn severity_critical() {
        assert_eq!(Severity::from_str_lossy("critical"), Severity::Critical);
    }

    #[test]
    fn severity_major() {
        assert_eq!(Severity::from_str_lossy("major"), Severity::Major);
    }

    #[test]
    fn severity_minor() {
        assert_eq!(Severity::from_str_lossy("minor"), Severity::Minor);
    }

    #[test]
    fn severity_info() {
        assert_eq!(Severity::from_str_lossy("info"), Severity::Info);
    }

    #[test]
    fn severity_case_insensitive() {
        assert_eq!(Severity::from_str_lossy("CRITICAL"), Severity::Critical);
        assert_eq!(Severity::from_str_lossy("Major"), Severity::Major);
        assert_eq!(Severity::from_str_lossy("MINOR"), Severity::Minor);
        assert_eq!(Severity::from_str_lossy("Info"), Severity::Info);
    }

    #[test]
    fn severity_unknown_falls_back_to_info() {
        assert_eq!(Severity::from_str_lossy("unknown"), Severity::Info);
        assert_eq!(Severity::from_str_lossy(""), Severity::Info);
        assert_eq!(Severity::from_str_lossy("foobar"), Severity::Info);
    }

    // ─── Severity ordering ───

    #[test]
    fn severity_ordering() {
        // Ord is by discriminant: Critical(0) < Major(1) < Minor(2) < Info(3)
        assert!(Severity::Critical < Severity::Major);
        assert!(Severity::Major < Severity::Minor);
        assert!(Severity::Minor < Severity::Info);
    }

    // ─── Severity display ───

    #[test]
    fn severity_display() {
        assert_eq!(format!("{}", Severity::Critical), "critical");
        assert_eq!(format!("{}", Severity::Major), "major");
        assert_eq!(format!("{}", Severity::Minor), "minor");
        assert_eq!(format!("{}", Severity::Info), "info");
    }

    #[test]
    fn severity_label() {
        assert_eq!(Severity::Critical.label(), "CRITICAL");
        assert_eq!(Severity::Major.label(), "MAJOR");
        assert_eq!(Severity::Minor.label(), "MINOR");
        assert_eq!(Severity::Info.label(), "INFO");
    }

    #[test]
    fn severity_icon() {
        assert!(!Severity::Critical.icon().is_empty());
        assert!(!Severity::Major.icon().is_empty());
        assert!(!Severity::Minor.icon().is_empty());
        assert!(!Severity::Info.icon().is_empty());
    }

    // ─── IssueType::from_str_lossy ───

    #[test]
    fn issue_type_security() {
        assert_eq!(IssueType::from_str_lossy("security"), IssueType::Security);
    }

    #[test]
    fn issue_type_sec_alias() {
        assert_eq!(IssueType::from_str_lossy("sec"), IssueType::Security);
    }

    #[test]
    fn issue_type_performance() {
        assert_eq!(
            IssueType::from_str_lossy("performance"),
            IssueType::Performance
        );
    }

    #[test]
    fn issue_type_perf_alias() {
        assert_eq!(IssueType::from_str_lossy("perf"), IssueType::Performance);
    }

    #[test]
    fn issue_type_bug() {
        assert_eq!(IssueType::from_str_lossy("bug"), IssueType::Bug);
    }

    #[test]
    fn issue_type_bugs_alias() {
        assert_eq!(IssueType::from_str_lossy("bugs"), IssueType::Bug);
    }

    #[test]
    fn issue_type_best_practice_variants() {
        assert_eq!(
            IssueType::from_str_lossy("best_practice"),
            IssueType::BestPractice
        );
        assert_eq!(
            IssueType::from_str_lossy("best-practice"),
            IssueType::BestPractice
        );
        assert_eq!(
            IssueType::from_str_lossy("bestpractice"),
            IssueType::BestPractice
        );
        assert_eq!(
            IssueType::from_str_lossy("best practice"),
            IssueType::BestPractice
        );
    }

    #[test]
    fn issue_type_style() {
        assert_eq!(IssueType::from_str_lossy("style"), IssueType::Style);
    }

    #[test]
    fn issue_type_formatting_alias() {
        assert_eq!(IssueType::from_str_lossy("formatting"), IssueType::Style);
    }

    #[test]
    fn issue_type_suggestion() {
        assert_eq!(
            IssueType::from_str_lossy("suggestion"),
            IssueType::Suggestion
        );
    }

    #[test]
    fn issue_type_info_alias() {
        assert_eq!(IssueType::from_str_lossy("info"), IssueType::Suggestion);
    }

    #[test]
    fn issue_type_unknown_falls_back() {
        assert_eq!(IssueType::from_str_lossy("xyz"), IssueType::Suggestion);
        assert_eq!(IssueType::from_str_lossy(""), IssueType::Suggestion);
    }

    // ─── IssueType display ───

    #[test]
    fn issue_type_display() {
        assert_eq!(format!("{}", IssueType::Security), "security");
        assert_eq!(format!("{}", IssueType::Performance), "performance");
        assert_eq!(format!("{}", IssueType::Bug), "bug");
        assert_eq!(format!("{}", IssueType::BestPractice), "best_practice");
        assert_eq!(format!("{}", IssueType::Style), "style");
        assert_eq!(format!("{}", IssueType::Suggestion), "suggestion");
    }

    // ─── LLMConfig::default ───

    #[test]
    fn llm_config_default() {
        let cfg = LLMConfig::default();
        assert!(cfg.api_key.is_empty());
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.temperature, 0.0);
        assert_eq!(cfg.max_tokens, 4096);
        assert_eq!(cfg.max_tokens_param, "max_tokens");
        assert_eq!(cfg.timeout, 600);
    }

    // ─── TokenUsage::default ───

    #[test]
    fn token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert!((usage.estimated_cost_usd - 0.0).abs() < f64::EPSILON);
    }

    // ─── Exit codes ───

    #[test]
    fn exit_codes_are_correct() {
        assert_eq!(EXIT_OK, 0);
        assert_eq!(EXIT_ERROR, 1);
        assert_eq!(EXIT_BLOCKED, 2);
        assert_eq!(EXIT_AUTH_ERROR, 3);
    }

    // ─── Constants ───

    #[test]
    fn max_diff_size() {
        assert_eq!(MAX_DIFF_SIZE, 50 * 1024);
    }

    #[test]
    fn max_scan_batch_files() {
        assert_eq!(MAX_SCAN_BATCH_FILES, 20);
    }

    #[test]
    fn max_scan_batch_chars() {
        assert_eq!(MAX_SCAN_BATCH_CHARS, 80_000);
    }

    // ─── ReviewIssue serde round-trip ───

    #[test]
    fn review_issue_without_rule_id_deserializes() {
        // JSON written before #597 (cache, history, MCP clients) has no rule_id.
        let old = r#"{"file":"a.rs","line":1,"severity":"major","issue_type":"rule","title":"T","body":"B","suggested_fix":null}"#;
        let issue: ReviewIssue = serde_json::from_str(old).unwrap();
        assert!(issue.rule_id.is_none());
        assert!(issue.also_matches.is_empty());
    }

    #[test]
    fn rule_id_serialized_only_when_present_and_aliases_never() {
        let mut issue: ReviewIssue = serde_json::from_str(
            r#"{"file":"a.rs","severity":"major","issue_type":null,"title":"T","body":"B"}"#,
        )
        .unwrap();
        let none = serde_json::to_string(&issue).unwrap();
        assert!(!none.contains("rule_id"));
        issue.rule_id = Some("sec-x".into());
        issue.also_matches = vec!["hidden".into()];
        let some = serde_json::to_string(&issue).unwrap();
        assert!(some.contains(r#""rule_id":"sec-x""#));
        assert!(!some.contains("also_matches") && !some.contains("hidden"));
        let back: ReviewIssue = serde_json::from_str(&some).unwrap();
        assert_eq!(back.rule_id.as_deref(), Some("sec-x"));
    }

    #[test]
    fn review_issue_roundtrip() {
        let issue = ReviewIssue::new("src/main.rs", Some(42), Severity::Critical, "SQL Injection")
            .with_type("security")
            .with_body("Details here")
            .with_fix("Use params");
        let json = serde_json::to_string(&issue).unwrap();
        let back: ReviewIssue = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file, issue.file);
        assert_eq!(back.line, issue.line);
        assert_eq!(back.severity, issue.severity);
        assert_eq!(back.issue_type, issue.issue_type);
        assert_eq!(back.title, issue.title);
        assert_eq!(back.body, issue.body);
        assert_eq!(back.suggested_fix, issue.suggested_fix);
    }

    #[test]
    fn review_issue_with_type_alias_deserializes() {
        let json = r#"{"file":"a.rs","severity":"info","type":"security","title":"T","body":"B"}"#;
        let issue: ReviewIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.issue_type.as_deref(), Some("security"));
    }

    #[test]
    fn review_issue_with_issue_type_deserializes() {
        let json = r#"{"file":"a.rs","severity":"info","issue_type":"performance","title":"T","body":"B"}"#;
        let issue: ReviewIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.issue_type.as_deref(), Some("performance"));
    }
}

/// Token usage tracking
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub estimated_cost_usd: f64,
}

/// Response from a code review
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResponse {
    pub issues: Vec<ReviewIssue>,
    pub summary: String,
    pub tokens_used: Option<TokenUsage>,
    pub should_block: bool,
    /// Deterministic findings cut by `rules_engine.max_findings` after
    /// suppression (#624). Never serialized, so JSON/SARIF output is
    /// unchanged; the CLI reports it on stderr.
    #[serde(skip)]
    pub dropped_findings: usize,
}

/// Response from a full project scan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResponse {
    pub issues: Vec<ReviewIssue>,
    pub summary: String,
    pub files_scanned: usize,
    #[serde(default)]
    pub lines_scanned: usize,
    pub tokens_used: Option<TokenUsage>,
    pub should_block: bool,
}

/// LLM provider configuration
#[derive(Debug, Clone)]
pub struct LLMConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub provider: String,
    pub temperature: f32,
    pub max_tokens: u32,
    /// JSON parameter name for max tokens (resolved from config).
    pub max_tokens_param: String,
    pub timeout: u64,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "gpt-4o-mini".to_string(),
            provider: "openai".to_string(),
            temperature: 0.0,
            max_tokens: 4096,
            max_tokens_param: "max_tokens".to_string(),
            timeout: 600,
        }
    }
}

/// CLI exit codes.
#[allow(dead_code)]
pub const EXIT_OK: i32 = 0;
#[allow(dead_code)]
pub const EXIT_ERROR: i32 = 1;
#[allow(dead_code)]
pub const EXIT_BLOCKED: i32 = 2;
#[allow(dead_code)]
pub const EXIT_AUTH_ERROR: i32 = 3;

/// Maximum diff size in bytes (50KB by default).
///
/// Kept for API completeness — future commands may enforce this limit.
#[allow(dead_code)]
pub const MAX_DIFF_SIZE: usize = 50 * 1024;

/// Maximum files per scan batch.
///
/// Kept for API completeness — scanner uses matching values inline.
#[allow(dead_code)]
pub const MAX_SCAN_BATCH_FILES: usize = 20;

/// Maximum characters per scan batch.
///
/// Kept for API completeness — scanner uses matching values inline.
#[allow(dead_code)]
pub const MAX_SCAN_BATCH_CHARS: usize = 80_000;
