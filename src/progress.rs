//! Structured NDJSON progress reporting for the `cora review` command.
//!
//! When `--progress` is set, progress events are written to stderr as one JSON object
//! per line (NDJSON). When not set, the reporter is a no-op with zero overhead.

use std::io::Write;

use chrono::Utc;
use serde::Serialize;

/// NDJSON progress event emitted to stderr.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Review execution started.
    Started {
        mode: String,
        base: Option<String>,
        timestamp: String,
    },
    /// Diff has been parsed and stats are available.
    ParsingDiff {
        files_changed: usize,
        lines_changed: usize,
        timestamp: String,
    },
    /// About to call the LLM.
    CallingLlm {
        provider: String,
        model: String,
        timestamp: String,
    },
    /// LLM responded.
    LlmResponse {
        tokens: TokenInfo,
        duration_ms: u64,
        timestamp: String,
    },
    /// Review completed successfully.
    Complete {
        issues: usize,
        blocked: bool,
        tokens: TokenInfo,
        timestamp: String,
    },
    /// An error occurred.
    Error {
        message: String,
        phase: String,
        timestamp: String,
    },
}

/// Token usage information for progress events.
#[derive(Debug, Clone, Serialize)]
pub struct TokenInfo {
    pub input: u32,
    pub output: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

impl TokenInfo {
    pub fn from_usage(usage: &crate::engine::types::TokenUsage) -> Self {
        TokenInfo {
            input: usage.input_tokens,
            output: usage.output_tokens,
            estimated_cost_usd: if usage.estimated_cost_usd > 0.0 {
                Some(usage.estimated_cost_usd)
            } else {
                None
            },
        }
    }

    pub fn zero() -> Self {
        TokenInfo {
            input: 0,
            output: 0,
            estimated_cost_usd: None,
        }
    }
}

/// A progress reporter that emits NDJSON events to stderr.
///
/// Create with `ProgressReporter::new()` when `--progress` is enabled,
/// or use `ProgressReporter::disabled()` (the default) for a no-op.
#[derive(Debug)]
pub struct ProgressReporter {
    enabled: bool,
}

impl ProgressReporter {
    /// Create a no-op progress reporter (when `--progress` is not set).
    pub fn disabled() -> Self {
        ProgressReporter { enabled: false }
    }

    /// Create an active progress reporter (when `--progress` is set).
    pub fn new() -> Self {
        ProgressReporter { enabled: true }
    }

    /// Returns true if this reporter is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    fn emit(&self, event: &ProgressEvent) {
        if !self.enabled {
            return;
        }
        if let Ok(json) = serde_json::to_string(event) {
            let _ = writeln!(std::io::stderr(), "{json}");
        }
    }

    /// Emit a "started" event.
    pub fn started(&self, mode: &str, base: Option<&str>) {
        self.emit(&ProgressEvent::Started {
            mode: mode.to_string(),
            base: base.map(std::string::ToString::to_string),
            timestamp: Self::now_iso(),
        });
    }

    /// Emit a "`parsing_diff`" event.
    pub fn parsing_diff(&self, files_changed: usize, lines_changed: usize) {
        self.emit(&ProgressEvent::ParsingDiff {
            files_changed,
            lines_changed,
            timestamp: Self::now_iso(),
        });
    }

    /// Emit a "`calling_llm`" event.
    pub fn calling_llm(&self, provider: &str, model: &str) {
        self.emit(&ProgressEvent::CallingLlm {
            provider: provider.to_string(),
            model: model.to_string(),
            timestamp: Self::now_iso(),
        });
    }

    /// Emit a "`llm_response`" event.
    pub fn llm_response(&self, tokens: &TokenInfo, duration_ms: u64) {
        self.emit(&ProgressEvent::LlmResponse {
            tokens: tokens.clone(),
            duration_ms,
            timestamp: Self::now_iso(),
        });
    }

    /// Emit a "complete" event.
    pub fn complete(&self, issues: usize, blocked: bool, tokens: &TokenInfo) {
        self.emit(&ProgressEvent::Complete {
            issues,
            blocked,
            tokens: tokens.clone(),
            timestamp: Self::now_iso(),
        });
    }

    /// Emit an "error" event.
    pub fn error(&self, message: &str, phase: &str) {
        self.emit(&ProgressEvent::Error {
            message: message.to_string(),
            phase: phase.to_string(),
            timestamp: Self::now_iso(),
        });
    }
}

/// Extract diff statistics: (`files_changed`, `lines_changed`).
pub fn diff_stats(diff: &str) -> (usize, usize) {
    let mut files_changed = 0;
    let mut lines_changed = 0;

    for line in diff.lines() {
        let trimmed = line.trim_start();
        // Count file headers from diff output
        if trimmed.starts_with("--- ") || trimmed.starts_with("+++ ") {
            // Only count once per file pair (the --- line)
            if trimmed.starts_with("--- ") {
                files_changed += 1;
            }
        }
        // Count changed lines (lines starting with + or - but not +++ or ---)
        if (trimmed.starts_with('+') && !trimmed.starts_with("++"))
            || (trimmed.starts_with('-') && !trimmed.starts_with("--"))
        {
            lines_changed += 1;
        }
    }

    // files_changed is double-counted from --- lines (each pair has one ---),
    // which is correct since we only increment on ---.
    (files_changed, lines_changed)
}

/// [`LlmEvents`](crate::engine::llm::LlmEvents) that streams model output to
/// stdout as it arrives. The LLM layer itself never prints; commands that want
/// live token output (`review --stream`, `commit --stream`) pass this in.
pub struct StdoutStream;

impl crate::engine::llm::LlmEvents for StdoutStream {
    fn delta(&self, chunk: &str) {
        print!("{chunk}");
        let _ = std::io::stdout().flush();
    }

    fn retry(&self) {
        // Separate the discarded (unparseable) output from the retried stream.
        eprintln!("\n[response was not valid JSON - retrying with a stricter prompt]");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_stats_empty() {
        let (files, lines) = diff_stats("");
        assert_eq!(files, 0);
        assert_eq!(lines, 0);
    }

    #[test]
    fn diff_stats_single_file() {
        let diff = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"world\");
+    let x = 1;
 }
";
        let (files, lines) = diff_stats(diff);
        assert_eq!(files, 1);
        assert_eq!(lines, 3); // -1 +2 +1
    }

    #[test]
    fn diff_stats_multiple_files() {
        let diff = "\
--- a/file1.rs
+++ b/file1.rs
@@ -1,3 +1,3 @@
- old
+ new
--- a/file2.rs
+++ b/file2.rs
@@ -1,3 +1,4 @@
- foo
+ bar
+ baz
";
        let (files, lines) = diff_stats(diff);
        assert_eq!(files, 2);
        assert_eq!(lines, 5); // -1 +1 -1 +1 +1
    }

    #[test]
    fn disabled_reporter_no_output() {
        let reporter = ProgressReporter::disabled();
        assert!(!reporter.is_enabled());
        // These should not panic or write anything
        reporter.started("review", None);
        reporter.parsing_diff(1, 10);
        reporter.calling_llm("openai", "gpt-4o-mini");
        reporter.llm_response(&TokenInfo::zero(), 100);
        reporter.complete(0, false, &TokenInfo::zero());
        reporter.error("test error", "test_phase");
    }

    #[test]
    fn enabled_reporter_is_enabled() {
        let reporter = ProgressReporter::new();
        assert!(reporter.is_enabled());
    }

    #[test]
    fn progress_event_started_serializes() {
        let event = ProgressEvent::Started {
            mode: "review".to_string(),
            base: Some("origin/develop".to_string()),
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "started");
        assert_eq!(parsed["mode"], "review");
        assert_eq!(parsed["base"], "origin/develop");
    }

    #[test]
    fn progress_event_parsing_diff_serializes() {
        let event = ProgressEvent::ParsingDiff {
            files_changed: 12,
            lines_changed: 340,
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "parsing_diff");
        assert_eq!(parsed["files_changed"], 12);
        assert_eq!(parsed["lines_changed"], 340);
    }

    #[test]
    fn progress_event_calling_llm_serializes() {
        let event = ProgressEvent::CallingLlm {
            provider: "openai".to_string(),
            model: "gpt-4o-mini".to_string(),
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "calling_llm");
        assert_eq!(parsed["provider"], "openai");
        assert_eq!(parsed["model"], "gpt-4o-mini");
    }

    #[test]
    fn progress_event_llm_response_serializes() {
        let event = ProgressEvent::LlmResponse {
            tokens: TokenInfo {
                input: 8200,
                output: 2400,
                estimated_cost_usd: Some(0.003),
            },
            duration_ms: 3200,
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "llm_response");
        assert_eq!(parsed["tokens"]["input"], 8200);
        assert_eq!(parsed["tokens"]["output"], 2400);
        assert_eq!(parsed["duration_ms"], 3200);
    }

    #[test]
    fn progress_event_complete_serializes() {
        let event = ProgressEvent::Complete {
            issues: 3,
            blocked: false,
            tokens: TokenInfo {
                input: 8200,
                output: 2400,
                estimated_cost_usd: Some(0.003),
            },
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "complete");
        assert_eq!(parsed["issues"], 3);
        assert_eq!(parsed["blocked"], false);
    }

    #[test]
    fn progress_event_error_serializes() {
        let event = ProgressEvent::Error {
            message: "API timeout after 120s".to_string(),
            phase: "calling_llm".to_string(),
            timestamp: "2026-06-02T10:00:00+00:00".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["message"], "API timeout after 120s");
        assert_eq!(parsed["phase"], "calling_llm");
    }

    #[test]
    fn token_info_zero() {
        let tokens = TokenInfo::zero();
        assert_eq!(tokens.input, 0);
        assert_eq!(tokens.output, 0);
        assert!(tokens.estimated_cost_usd.is_none());
    }

    #[test]
    fn token_info_serializes_without_cost_when_zero() {
        let tokens = TokenInfo::zero();
        let json = serde_json::to_string(&tokens).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            !parsed
                .as_object()
                .unwrap()
                .contains_key("estimated_cost_usd")
        );
    }

    #[test]
    fn token_info_serializes_with_cost_when_nonzero() {
        let tokens = TokenInfo {
            input: 100,
            output: 50,
            estimated_cost_usd: Some(0.001),
        };
        let json = serde_json::to_string(&tokens).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["estimated_cost_usd"], 0.001);
    }
}
