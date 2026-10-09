use anyhow::Result;
use colored::Colorize;

use crate::engine::{ReviewIssue, ReviewResponse, ScanResponse, Severity};
use crate::formatters::Formatter;

/// Compact formatter: one line per issue. Ideal for git hooks.
pub struct CompactFormatter;

#[allow(clippy::format_push_string)]
impl Formatter for CompactFormatter {
    fn format_review(&self, response: &ReviewResponse) -> Result<String> {
        let mut output = String::new();

        if response.issues.is_empty() {
            output.push_str("No issues found.\n");
            return Ok(output);
        }

        output.push_str(&format!(
            "Found {} issue{}.\n",
            response.issues.len(),
            if response.issues.len() == 1 { "" } else { "s" },
        ));

        for issue in &response.issues {
            output.push_str(&format_issue_compact(issue));
        }

        // Footer watermark (only when issues found)
        output.push_str(&format!(
            "Reviewed by Cora v{}\n",
            env!("CARGO_PKG_VERSION")
        ));

        Ok(output)
    }

    fn format_scan(&self, response: &ScanResponse) -> Result<String> {
        let mut output = String::new();

        output.push_str(&format!(
            "Scanned {} files — Found {} issue{}.\n",
            response.files_scanned,
            response.issues.len(),
            if response.issues.len() == 1 { "" } else { "s" },
        ));

        for issue in &response.issues {
            output.push_str(&format_issue_compact(issue));
        }

        // Footer watermark (only when issues found)
        if !response.issues.is_empty() {
            output.push_str(&format!(
                "Reviewed by Cora v{}\n",
                env!("CARGO_PKG_VERSION")
            ));
        }

        Ok(output)
    }
}

/// Format a single issue as one line: [SEVERITY] <file:line>: title
fn format_issue_compact(issue: &ReviewIssue) -> String {
    let sev = match issue.severity {
        Severity::Critical => "CRITICAL".red().bold().to_string(),
        Severity::Major => "MAJOR".yellow().bold().to_string(),
        Severity::Minor => "MINOR".blue().to_string(),
        Severity::Info => "INFO".white().dimmed().to_string(),
    };

    let loc = match issue.line {
        Some(l) => format!("{}:{}", issue.file, l),
        None => issue.file.clone(),
    };

    match issue.rule_id {
        Some(ref id) => format!("[{sev}] {loc}: {} ({id})\n", issue.title),
        None => format!("[{sev}] {loc}: {}\n", issue.title),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ReviewIssue, ScanResponse, Severity};

    fn sample_issue() -> ReviewIssue {
        ReviewIssue::new("src/main.rs", Some(42), Severity::Critical, "SQL Injection")
            .with_type("security")
            .with_body("Details here.")
    }

    fn sample_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![sample_issue()],
            summary: "Summary".to_string(),
            tokens_used: None,
            should_block: false,
            dropped_findings: 0,
        }
    }

    fn empty_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![],
            summary: String::new(),
            tokens_used: None,
            should_block: false,
            dropped_findings: 0,
        }
    }

    fn sample_scan_response() -> ScanResponse {
        ScanResponse {
            issues: vec![sample_issue()],
            summary: String::new(),
            files_scanned: 10,
            lines_scanned: 500,
            tokens_used: None,
            should_block: false,
        }
    }

    #[test]
    fn format_review_contains_issue_count() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("Found 1 issue"));
    }

    #[test]
    fn format_review_contains_file() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("src/main.rs"));
    }

    #[test]
    fn format_review_contains_title() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("SQL Injection"));
    }

    #[test]
    fn format_review_empty_issues() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&empty_response()).unwrap();
        assert!(output.contains("No issues found"));
        // No watermark when no issues
        assert!(!output.contains("Reviewed by Cora"));
    }

    #[test]
    fn format_review_with_issues_has_watermark() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("Reviewed by Cora v"));
    }

    #[test]
    fn format_review_contains_severity() {
        let fmt = CompactFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("CRITICAL"));
    }

    #[test]
    fn format_scan_contains_file_count() {
        let fmt = CompactFormatter;
        let output = fmt.format_scan(&sample_scan_response()).unwrap();
        assert!(output.contains("10 files"));
    }

    #[test]
    fn format_issue_compact_no_line() {
        let issue = ReviewIssue::new("README.md", None, Severity::Info, "Typo");
        let line = format_issue_compact(&issue);
        assert!(line.contains("README.md"));
        // When no line, file is directly followed by ": title" not "file:N: title"
        // Output is "[INFO] README.md: Typo\n"
        assert!(!line.contains("README.md:1"));
    }

    #[test]
    fn format_issue_compact_with_line() {
        let line = format_issue_compact(&sample_issue());
        assert!(line.contains("src/main.rs:42"));
    }
}
