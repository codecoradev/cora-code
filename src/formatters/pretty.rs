use anyhow::Result;
use colored::Colorize;

use crate::engine::{ReviewIssue, ReviewResponse, ScanResponse, Severity};
use crate::formatters::Formatter;

/// Pretty formatter with colored terminal output and severity icons.
pub struct PrettyFormatter;

#[allow(clippy::format_push_string)]
impl Formatter for PrettyFormatter {
    fn format_review(&self, response: &ReviewResponse) -> Result<String> {
        let mut output = String::new();

        // Header
        output.push_str(&format!(
            "{}\n\n",
            "╔══════════════════════════════════════════╗
║          Cora Code Review Results        ║
╚══════════════════════════════════════════╝"
                .cyan()
                .bold()
        ));

        if response.issues.is_empty() {
            output.push_str(&"✅ No issues found!".green().bold().to_string());
            if !response.summary.is_empty() {
                output.push_str(&format!("\n\n{}", response.summary.dimmed()));
            }
            return Ok(output);
        }

        // Summary line
        let crit = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Critical)
            .count();
        let maj = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Major)
            .count();
        let min = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Minor)
            .count();
        let inf = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Info)
            .count();

        output.push_str(&format!(
            "Found {} issue{}: {} {} {} {} {}\n\n",
            response.issues.len(),
            if response.issues.len() == 1 { "" } else { "s" },
            format!("{crit} critical").red(),
            format!("{maj} major").yellow(),
            format!("{min} minor").blue(),
            format!("{inf} info").white(),
            if response.should_block {
                format!("\n{}", "⚠ BLOCKED".red().bold())
            } else {
                String::new()
            },
        ));

        // Issues
        for (idx, issue) in response.issues.iter().enumerate() {
            output.push_str(&format_issue_pretty(issue, idx + 1));
            output.push('\n');
        }

        // Summary
        if !response.summary.is_empty() {
            output.push_str(&format!(
                "\n{}\n{}",
                "─".repeat(60).dimmed(),
                response.summary.white().bold()
            ));
        }

        // Footer watermark (only when issues found)
        output.push_str(&format!(
            "\n{}\n{}",
            "─".repeat(60).dimmed(),
            format!("Reviewed by Cora v{}", env!("CARGO_PKG_VERSION")).dimmed()
        ));

        Ok(output)
    }

    fn format_scan(&self, response: &ScanResponse) -> Result<String> {
        let mut output = String::new();

        output.push_str(&format!(
            "{}\n",
            "╔══════════════════════════════════════════╗
║        Cora Project Scan Results           ║
╚══════════════════════════════════════════╝"
                .cyan()
                .bold()
        ));

        output.push_str(&format!(
            "Scanned {} files ({} lines)\n\n",
            response.files_scanned, response.lines_scanned
        ));

        if response.issues.is_empty() {
            output.push_str(&"✅ No issues found!".green().bold().to_string());
            if !response.summary.is_empty() {
                output.push_str(&format!("\n\n{}", response.summary.dimmed()));
            }
            return Ok(output);
        }

        let crit = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Critical)
            .count();
        let maj = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Major)
            .count();
        let min = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Minor)
            .count();
        let inf = response
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Info)
            .count();

        output.push_str(&format!(
            "Found {} issue{}: {} {} {} {} {}\n\n",
            response.issues.len(),
            if response.issues.len() == 1 { "" } else { "s" },
            format!("{crit} critical").red(),
            format!("{maj} major").yellow(),
            format!("{min} minor").blue(),
            format!("{inf} info").white(),
            if response.should_block {
                format!("\n{}", "⚠ BLOCKED".red().bold())
            } else {
                String::new()
            },
        ));

        for (idx, issue) in response.issues.iter().enumerate() {
            output.push_str(&format_issue_pretty(issue, idx + 1));
            output.push('\n');
        }

        if !response.summary.is_empty() {
            output.push_str(&format!(
                "\n{}\n{}",
                "─".repeat(60).dimmed(),
                response.summary.white().bold()
            ));
        }

        // Footer watermark (only when issues found)
        if !response.issues.is_empty() {
            output.push_str(&format!(
                "\n{}\n{}",
                "─".repeat(60).dimmed(),
                format!("Reviewed by Cora v{}", env!("CARGO_PKG_VERSION")).dimmed()
            ));
        }

        Ok(output)
    }
}

/// Format a single issue with colors and icons.
#[allow(clippy::format_push_string)]
fn format_issue_pretty(issue: &ReviewIssue, num: usize) -> String {
    let mut out = String::new();

    // Header: icon + severity + type
    let icon = issue.severity.icon();
    let sev_colored = match issue.severity {
        Severity::Critical => issue.severity.label().red().bold(),
        Severity::Major => issue.severity.label().yellow().bold(),
        Severity::Minor => issue.severity.label().blue().bold(),
        Severity::Info => issue.severity.label().white().bold(),
    };

    out.push_str(&format!("{icon} [{sev_colored}] "));
    if let Some(ref itype) = issue.issue_type {
        out.push_str(&format!("{itype}: "));
    }

    // File:line location
    let loc = match issue.line {
        Some(l) => format!("{}:{}", issue.file, l),
        None => issue.file.clone(),
    };
    out.push_str(&loc.white().bold().to_string());
    out.push('\n');

    // Title
    out.push_str(&format!(
        "  {} {}",
        format!("#{num}").dimmed(),
        issue.title.bold()
    ));
    // Rule id, usable in `cora-ignore:` / `ignore.rules` (#597).
    if let Some(ref id) = issue.rule_id {
        out.push_str(&format!(" {}", format!("(rule: {id})").dimmed()));
    }
    out.push('\n');

    // Body
    if !issue.body.is_empty() {
        for line in issue.body.lines().take(10) {
            out.push_str(&format!("    {}\n", line.dimmed()));
        }
    }

    // Suggested fix
    if let Some(ref fix) = issue.suggested_fix {
        if !fix.trim().is_empty() {
            out.push_str(&format!("  {}\n", "💡 Suggested fix:".green().bold()));
            out.push_str(&format!("    {fix}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ReviewIssue, ScanResponse, Severity};

    fn sample_issue() -> ReviewIssue {
        ReviewIssue::new("src/main.rs", Some(42), Severity::Critical, "SQL Injection")
            .with_type("security")
            .with_body("User input is concatenated directly into SQL query.")
            .with_fix("Use parameterized queries.")
    }

    fn sample_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![sample_issue()],
            summary: "Found 1 critical issue.".to_string(),
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

    fn blocked_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![sample_issue()],
            summary: String::new(),
            tokens_used: None,
            should_block: true,
            dropped_findings: 0,
        }
    }

    fn sample_scan_response() -> ScanResponse {
        ScanResponse {
            issues: vec![sample_issue()],
            summary: "Scan done.".to_string(),
            files_scanned: 10,
            lines_scanned: 500,
            tokens_used: None,
            should_block: false,
        }
    }

    #[test]
    fn format_review_contains_file() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("src/main.rs"));
    }

    #[test]
    fn format_review_contains_title() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("SQL Injection"));
    }

    #[test]
    fn format_review_contains_header() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("Cora Code Review Results"));
    }

    #[test]
    fn format_review_empty_issues_shows_no_issues() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&empty_response()).unwrap();
        assert!(output.contains("No issues found"));
        // No watermark when no issues
        assert!(!output.contains("Reviewed by Cora"));
    }

    #[test]
    fn format_review_with_issues_has_watermark() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("Reviewed by Cora v"));
    }

    #[test]
    fn format_scan_with_issues_has_watermark() {
        let fmt = PrettyFormatter;
        let output = fmt.format_scan(&sample_scan_response()).unwrap();
        assert!(output.contains("Reviewed by Cora v"));
    }

    #[test]
    fn format_review_with_summary() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("Found 1 critical issue."));
    }

    #[test]
    fn format_review_blocked_shows_blocked() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&blocked_response()).unwrap();
        assert!(output.contains("BLOCKED"));
    }

    #[test]
    fn format_review_issue_count() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("1 issue"));
    }

    #[test]
    fn format_review_issue_type_displayed() {
        let fmt = PrettyFormatter;
        let output = fmt.format_review(&sample_response()).unwrap();
        assert!(output.contains("security"));
    }

    #[test]
    fn format_scan_contains_header() {
        let fmt = PrettyFormatter;
        let output = fmt.format_scan(&sample_scan_response()).unwrap();
        assert!(output.contains("Cora Project Scan Results"));
    }

    #[test]
    fn format_scan_contains_file_line_count() {
        let fmt = PrettyFormatter;
        let output = fmt.format_scan(&sample_scan_response()).unwrap();
        assert!(output.contains("10 files"));
        assert!(output.contains("500 lines"));
    }

    #[test]
    fn format_issue_pretty_no_line() {
        let issue = ReviewIssue::new("README.md", None, Severity::Info, "Typo");
        let out = format_issue_pretty(&issue, 1);
        assert!(out.contains("README.md"));
        assert!(!out.contains("README.md:"));
    }

    #[test]
    fn format_issue_pretty_with_fix() {
        let out = format_issue_pretty(&sample_issue(), 1);
        assert!(out.contains("Suggested fix"));
        assert!(out.contains("parameterized"));
    }
}
