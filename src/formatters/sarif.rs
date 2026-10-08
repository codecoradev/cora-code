use anyhow::Result;
use serde_json::{Value, json};

use crate::engine::{ReviewIssue, ReviewResponse, ScanResponse, Severity};
use crate::formatters::Formatter;

/// SARIF formatter for GitHub Code Scanning integration.
///
/// Output conforms to the SARIF v2.1.0 specification:
/// <https://docs.oasis-open.org/sarif/sarif/v2.1.0>/
pub struct SarifFormatter;

impl Formatter for SarifFormatter {
    fn format_review(&self, response: &ReviewResponse) -> Result<String> {
        let sarif = build_sarif(&response.issues);
        let json = serde_json::to_string_pretty(&sarif)?;
        Ok(json)
    }

    fn format_scan(&self, response: &ScanResponse) -> Result<String> {
        let sarif = build_sarif(&response.issues);
        let json = serde_json::to_string_pretty(&sarif)?;
        Ok(json)
    }
}

/// Build a SARIF v2.1.0 document from a list of issues.
fn build_sarif(issues: &[ReviewIssue]) -> Value {
    // Deduplicate rules by id (multiple issues can share same rule)
    let mut rule_map = serde_json::Map::new();
    for issue in issues {
        let rule_id = issue
            .issue_type
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if !rule_map.contains_key(&rule_id) {
            rule_map.insert(
                rule_id.clone(),
                json!({
                    "id": rule_id,
                    "shortDescription": {
                        "text": issue.title.clone()
                    },
                    "fullDescription": {
                        "text": issue.body.clone()
                    },
                    "defaultConfiguration": {
                        "level": severity_to_sarif_level(&issue.severity)
                    }
                }),
            );
        }
    }
    let rules: Vec<Value> = rule_map.into_values().collect();

    let results: Vec<Value> = issues
        .iter()
        .map(|issue| {
            let mut result = json!({
                "ruleId": issue.issue_type.clone().unwrap_or_else(|| "unknown".to_string()),
                "level": severity_to_sarif_level(&issue.severity),
                "message": {
                    "text": issue.body.clone()
                },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": {
                            "uri": issue.file.clone()
                        }
                    }
                }]
            });

            if let Some(ref id) = issue.rule_id {
                result["properties"] = json!({ "coraRuleId": id });
            }

            // Add line number if available
            if let Some(line) = issue.line {
                result["locations"][0]["physicalLocation"]["region"] = json!({
                    "startLine": line
                });
            }

            // Add fix suggestion if available
            if let Some(ref fix) = issue.suggested_fix {
                result["fixes"] = json!([{
                    "description": {
                        "text": fix.clone()
                    },
                    "artifactChanges": [{
                        "artifactLocation": {
                            "uri": issue.file.clone()
                        },
                        "replacements": [{
                            "deletedRegion": {
                                "startLine": issue.line.unwrap_or(1)
                            },
                            "insertedContent": {
                                "text": fix.clone()
                            }
                        }]
                    }]
                }]);
            }

            result
        })
        .collect();

    // Build invocations array with watermark
    let invocations = if issues.is_empty() {
        json!([])
    } else {
        json!([{
            "executionSuccessful": true,
            "properties": {
                "cora.watermark": format!("Reviewed by CodeCora v{}", env!("CARGO_PKG_VERSION"))
            }
        }])
    };

    json!({
        "$schema": "https://raw.githubusercontent.com/oasis-tcs/sarif-spec/master/Schemata/sarif-schema-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "CodeCora",
                    "fullName": "codecoradev/cora-code",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": env!("CARGO_PKG_REPOSITORY"),
                    "rules": rules
                }
            },
            "results": results,
            "invocations": invocations
        }]
    })
}

/// Map our severity to SARIF failure level.
fn severity_to_sarif_level(severity: &Severity) -> &str {
    match severity {
        Severity::Critical | Severity::Major => "error",
        Severity::Minor => "warning",
        Severity::Info => "note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ReviewIssue, Severity};

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
            tokens_used: None,
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

    #[test]
    fn sarif_output_is_valid_json() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn sarif_has_required_schema_field() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed["$schema"].as_str().unwrap().contains("sarif-schema"));
    }

    #[test]
    fn sarif_has_version() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["version"].as_str().unwrap(), "2.1.0");
    }

    #[test]
    fn sarif_has_runs() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let runs = parsed["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn sarif_tool_driver_name_is_codecora() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["runs"][0]["tool"]["driver"]["name"]
                .as_str()
                .unwrap(),
            "CodeCora"
        );
    }

    #[test]
    fn sarif_tool_driver_has_fullname_codecoradev() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["runs"][0]["tool"]["driver"]["fullName"]
                .as_str()
                .unwrap(),
            "codecoradev/cora-code"
        );
    }

    #[test]
    fn sarif_results_contain_issue() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["level"].as_str().unwrap(), "error"); // critical → error
    }

    #[test]
    fn sarif_empty_issues_produces_valid_output() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&empty_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let results = parsed["runs"][0]["results"].as_array().unwrap();
        assert!(results.is_empty());
        // No watermark invocations when no issues
        let invocations = parsed["runs"][0]["invocations"].as_array().unwrap();
        assert!(invocations.is_empty());
    }

    #[test]
    fn sarif_with_issues_has_watermark_invocation() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let invocations = parsed["runs"][0]["invocations"].as_array().unwrap();
        assert_eq!(invocations.len(), 1);
        let wm = &invocations[0]["properties"]["cora.watermark"];
        let wm_str = wm.as_str().unwrap();
        assert!(wm_str.contains("Cora"));
        assert!(wm_str.contains('v'));
    }

    #[test]
    fn sarif_location_includes_line() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let region = &parsed["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"].as_u64().unwrap(), 42);
    }

    #[test]
    fn sarif_scan_format_also_valid() {
        let fmt = SarifFormatter;
        let scan = ScanResponse {
            issues: vec![sample_issue()],
            summary: "Done.".to_string(),
            files_scanned: 5,
            lines_scanned: 200,
            tokens_used: None,
            should_block: false,
        };
        let output = fmt.format_scan(&scan).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert!(parsed["$schema"].as_str().is_some());
        assert_eq!(parsed["runs"][0]["results"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn sarif_severity_to_level_critical_is_error() {
        assert_eq!(severity_to_sarif_level(&Severity::Critical), "error");
    }

    #[test]
    fn sarif_severity_to_level_major_is_error() {
        assert_eq!(severity_to_sarif_level(&Severity::Major), "error");
    }

    #[test]
    fn sarif_severity_to_level_minor_is_warning() {
        assert_eq!(severity_to_sarif_level(&Severity::Minor), "warning");
    }

    #[test]
    fn sarif_severity_to_level_info_is_note() {
        assert_eq!(severity_to_sarif_level(&Severity::Info), "note");
    }

    #[test]
    fn sarif_issue_without_line_has_no_region() {
        let issue = ReviewIssue {
            rule_id: None,
            also_matches: Vec::new(),
            file: "src/lib.rs".to_string(),
            line: None,
            severity: Severity::Info,
            issue_type: None,
            title: "No line".to_string(),
            body: "No line info.".to_string(),
            suggested_fix: None,
        };
        let response = ReviewResponse {
            issues: vec![issue],
            summary: String::new(),
            tokens_used: None,
            should_block: false,
        };
        let fmt = SarifFormatter;
        let output = fmt.format_review(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let result = &parsed["runs"][0]["results"][0];
        let loc = &result["locations"][0]["physicalLocation"];
        assert!(loc.get("region").is_none());
    }

    #[test]
    fn sarif_issue_with_fix() {
        let fmt = SarifFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let result = &parsed["runs"][0]["results"][0];
        assert!(result.get("fixes").is_some());
    }
}
