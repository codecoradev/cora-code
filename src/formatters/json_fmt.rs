use anyhow::Result;
use serde_json::{Value, json};

use crate::engine::{ReviewResponse, ScanResponse};
use crate::formatters::Formatter;

/// JSON formatter: outputs raw JSON with a watermark footer.
pub struct JsonFormatter;

impl Formatter for JsonFormatter {
    fn format_review(&self, response: &ReviewResponse) -> Result<String> {
        let mut obj = serde_json::to_value(response)?;
        add_watermark(&mut obj, &response.issues);
        let json = serde_json::to_string_pretty(&obj)?;
        Ok(json)
    }

    fn format_scan(&self, response: &ScanResponse) -> Result<String> {
        let mut obj = serde_json::to_value(response)?;
        add_watermark_scan(&mut obj, &response.issues);
        let json = serde_json::to_string_pretty(&obj)?;
        Ok(json)
    }
}

/// Add a `reviewed_by` watermark field to a JSON value when issues are present.
fn add_watermark(obj: &mut Value, issues: &[crate::engine::ReviewIssue]) {
    if !issues.is_empty() {
        if let Some(map) = obj.as_object_mut() {
            map.insert(
                "reviewed_by".to_string(),
                json!({
                    "tool": "cora",
                    "version": env!("CARGO_PKG_VERSION")
                }),
            );
        }
    }
}

/// Add a `reviewed_by` watermark field for scan responses.
fn add_watermark_scan(obj: &mut Value, issues: &[crate::engine::ReviewIssue]) {
    add_watermark(obj, issues);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ReviewIssue, Severity, TokenUsage};

    fn sample_issue() -> ReviewIssue {
        ReviewIssue {
            rule_id: None,
            also_matches: Vec::new(),
            file: "src/main.rs".to_string(),
            line: Some(42),
            severity: Severity::Critical,
            issue_type: Some("security".to_string()),
            title: "SQL Injection".to_string(),
            body: "User input concatenated into query.".to_string(),
            suggested_fix: Some("Use parameterized queries.".to_string()),
        }
    }

    fn sample_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![sample_issue()],
            summary: "Found 1 critical issue.".to_string(),
            tokens_used: Some(TokenUsage {
                input_tokens: 100,
                output_tokens: 200,
                estimated_cost_usd: 0.005,
            }),
            should_block: false,
        }
    }

    fn empty_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![],
            summary: String::new(),
            tokens_used: None,
            should_block: false,
        }
    }

    fn sample_scan_response() -> ScanResponse {
        ScanResponse {
            issues: vec![sample_issue()],
            summary: "Scan complete.".to_string(),
            files_scanned: 10,
            lines_scanned: 500,
            tokens_used: None,
            should_block: false,
        }
    }

    #[test]
    fn format_review_output_is_valid_json() {
        let fmt = JsonFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed.get("issues").is_some());
        assert!(parsed.get("summary").is_some());
    }

    #[test]
    fn format_review_contains_issue_data() {
        let fmt = JsonFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("SQL Injection"));
        assert!(output.contains("critical"));
    }

    #[test]
    fn format_review_empty_issues() {
        let fmt = JsonFormatter;
        let output = fmt.format_review(&empty_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["issues"].as_array().unwrap().len(), 0);
        // No watermark when no issues
        assert!(parsed.get("reviewed_by").is_none());
    }

    #[test]
    fn format_review_with_issues_has_watermark() {
        let fmt = JsonFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        // Watermark should be present when issues exist
        let reviewed_by = &parsed["reviewed_by"];
        assert_eq!(reviewed_by["tool"].as_str().unwrap(), "cora");
        assert!(reviewed_by["version"].as_str().is_some());
    }

    #[test]
    fn format_scan_output_is_valid_json() {
        let fmt = JsonFormatter;
        let output = fmt.format_scan(&sample_scan_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed.get("files_scanned").is_some());
        assert!(parsed.get("lines_scanned").is_some());
        assert_eq!(parsed["files_scanned"].as_u64().unwrap(), 10);
    }

    #[test]
    fn format_review_multiple_issues() {
        let mut response = sample_response();
        response.issues.push(ReviewIssue {
            rule_id: None,
            also_matches: Vec::new(),
            file: "src/lib.rs".to_string(),
            line: Some(10),
            severity: Severity::Minor,
            issue_type: Some("style".to_string()),
            title: "Naming".to_string(),
            body: "Use snake_case.".to_string(),
            suggested_fix: None,
        });
        let fmt = JsonFormatter;
        let output = fmt.format_review(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["issues"].as_array().unwrap().len(), 2);
    }
}
