//! Quality Gate — evaluates review findings against configurable thresholds.
//!
//! Produces a definitive PASS/FAIL result that CI can use to gate merges.
//! Inspired by SonarQube Quality Gates.

use crate::engine::ReviewIssue;
use crate::engine::Severity;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Quality gate configuration — parsed from `.cora.yaml` under `quality_gate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateConfig {
    /// Enable quality gate evaluation.
    #[serde(default)]
    pub enabled: bool,

    /// Global thresholds — any exceeded = FAIL.
    #[serde(default)]
    pub thresholds: ThresholdConfig,

    /// Per-category action overrides.
    #[serde(default)]
    pub categories: HashMap<String, CategoryConfig>,
}

/// Global threshold configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdConfig {
    /// Max critical issues allowed (default: 0).
    #[serde(default = "default_max_critical")]
    pub max_critical: usize,

    /// Max major issues allowed (default: disabled / usize::MAX).
    #[serde(default = "default_disabled")]
    pub max_major: usize,

    /// Max minor issues allowed (default: disabled).
    #[serde(default = "default_disabled")]
    pub max_minor: usize,

    /// Max security findings allowed regardless of severity (default: 0).
    #[serde(default = "default_max_security")]
    pub max_security: usize,
}

fn default_max_critical() -> usize {
    0
}

fn default_max_security() -> usize {
    0
}

fn default_disabled() -> usize {
    usize::MAX
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            max_critical: 0,
            max_major: usize::MAX,
            max_minor: usize::MAX,
            max_security: 0,
        }
    }
}

/// Per-category gate action.
///
/// Deserialized case-insensitively from `.cora.yaml` so that `block`,
/// `Block`, and `BLOCK` are all accepted — but an unknown value like `blok`
/// fails loudly at config load instead of silently becoming blocking (#57).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CategoryAction {
    Block,
    #[default]
    Warn,
    Ignore,
}

impl CategoryAction {
    pub fn as_str(self) -> &'static str {
        match self {
            CategoryAction::Block => "block",
            CategoryAction::Warn => "warn",
            CategoryAction::Ignore => "ignore",
        }
    }

    /// Parse case-insensitively. Returns `None` for unknown values.
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "block" => Some(Self::Block),
            "warn" => Some(Self::Warn),
            "ignore" => Some(Self::Ignore),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for CategoryAction {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str_lossy(&s)
            .ok_or_else(|| serde::de::Error::unknown_variant(&s, &["block", "warn", "ignore"]))
    }
}

/// Per-category gate configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryConfig {
    /// Action: `block` (fail CI), `warn` (comment only), `ignore` (skip).
    #[serde(default)]
    pub action: CategoryAction,

    /// Max findings allowed in this category.
    #[serde(default = "default_disabled")]
    pub max_findings: usize,
}

/// Result of quality gate evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    /// Overall gate status: PASS or FAIL.
    pub status: GateStatus,

    /// Individual threshold check results.
    pub checks: Vec<CheckResult>,

    /// Counts by severity.
    pub severity_counts: SeverityCounts,

    /// Counts by issue type/category.
    pub category_counts: HashMap<String, usize>,

    /// Total findings evaluated.
    pub total_findings: usize,
}

/// Gate pass/fail status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GateStatus {
    Pass,
    Fail,
}

impl std::fmt::Display for GateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GateStatus::Pass => write!(f, "PASSED"),
            GateStatus::Fail => write!(f, "FAILED"),
        }
    }
}

/// A single threshold check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    /// What was checked.
    pub name: String,
    /// Threshold value.
    pub threshold: usize,
    /// Actual value.
    pub actual: usize,
    /// Did it pass?
    pub passed: bool,
    /// Category action (for category checks).
    pub action: Option<CategoryAction>,
}

/// Counts of findings by severity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeverityCounts {
    pub critical: usize,
    pub major: usize,
    pub minor: usize,
    pub info: usize,
}

/// Evaluate quality gate against a list of issues.
pub fn evaluate(issues: &[ReviewIssue], config: &QualityGateConfig) -> GateResult {
    // Count by severity
    let mut counts = SeverityCounts::default();
    for issue in issues {
        match issue.severity {
            Severity::Critical => counts.critical += 1,
            Severity::Major => counts.major += 1,
            Severity::Minor => counts.minor += 1,
            Severity::Info => counts.info += 1,
        }
    }

    // Count by category (issue_type)
    let mut category_counts: HashMap<String, usize> = HashMap::new();
    for issue in issues {
        if let Some(ref itype) = issue.issue_type {
            *category_counts.entry(itype.to_lowercase()).or_insert(0) += 1;
        }
    }

    // Count security findings (across all severities)
    let security_count = category_counts.get("security").copied().unwrap_or(0);

    // Evaluate global thresholds
    let mut checks = Vec::new();

    // Critical threshold
    checks.push(CheckResult {
        name: "max_critical".to_string(),
        threshold: config.thresholds.max_critical,
        actual: counts.critical,
        passed: counts.critical <= config.thresholds.max_critical,
        action: None,
    });

    // Major threshold
    if config.thresholds.max_major < usize::MAX {
        checks.push(CheckResult {
            name: "max_major".to_string(),
            threshold: config.thresholds.max_major,
            actual: counts.major,
            passed: counts.major <= config.thresholds.max_major,
            action: None,
        });
    }

    // Minor threshold
    if config.thresholds.max_minor < usize::MAX {
        checks.push(CheckResult {
            name: "max_minor".to_string(),
            threshold: config.thresholds.max_minor,
            actual: counts.minor,
            passed: counts.minor <= config.thresholds.max_minor,
            action: None,
        });
    }

    // Security threshold
    checks.push(CheckResult {
        name: "max_security".to_string(),
        threshold: config.thresholds.max_security,
        actual: security_count,
        passed: security_count <= config.thresholds.max_security,
        action: None,
    });

    // Evaluate per-category overrides
    for (category, cat_config) in &config.categories {
        if cat_config.action == CategoryAction::Ignore {
            continue;
        }
        let cat_count = category_counts.get(category).copied().unwrap_or(0);
        checks.push(CheckResult {
            name: format!("category:{}", category),
            threshold: cat_config.max_findings,
            actual: cat_count,
            passed: cat_count <= cat_config.max_findings,
            action: Some(cat_config.action),
        });
    }

    // Determine overall status:
    // #58: A disabled gate must never FAIL. Counts and checks are still
    // produced for reporting, but a disabled gate forces PASS.
    // Otherwise FAIL if any "block" category or global threshold is exceeded.
    // Global thresholds have action None (always fail when exceeded);
    // "warn" categories never fail the gate.
    let status = if !config.enabled {
        GateStatus::Pass
    } else if checks
        .iter()
        .any(|c| !c.passed && c.action != Some(CategoryAction::Warn))
    {
        GateStatus::Fail
    } else {
        GateStatus::Pass
    };

    GateResult {
        status,
        checks,
        severity_counts: counts,
        category_counts,
        total_findings: issues.len(),
    }
}

/// Format gate result as a terminal-friendly table.
#[allow(clippy::format_push_string)]
pub fn format_gate_output(result: &GateResult) -> String {
    use colored::Colorize;

    let mut out = String::new();

    let status_str = match result.status {
        GateStatus::Pass => "✅ PASSED".green().bold().to_string(),
        GateStatus::Fail => "❌ FAILED".red().bold().to_string(),
    };

    out.push_str(&format!(
        "\n{}\n",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    ));
    out.push_str(&format!("  {}\n", "QUALITY GATE RESULT".cyan().bold()));
    out.push_str(&format!(
        "{}\n",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    ));
    out.push_str(&format!("  Status:   {}\n", status_str));
    out.push_str(&format!(
        "  Findings: {} critical, {} major, {} minor, {} info\n",
        result.severity_counts.critical.to_string().red(),
        result.severity_counts.major.to_string().yellow(),
        result.severity_counts.minor.to_string().blue(),
        result.severity_counts.info,
    ));

    if !result.checks.is_empty() {
        out.push_str(&format!("\n  {}\n", "Threshold Checks:".white().bold()));
        for check in &result.checks {
            let icon = if check.passed { "✅" } else { "❌" };
            let status_label = if check.passed {
                "OK".green().to_string()
            } else {
                "EXCEEDED".red().bold().to_string()
            };
            let action_label = check
                .action
                .map(|a| format!(" ({})", a.as_str().dimmed()))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {} {:<20} → {} found   {}{}\n",
                icon,
                check.name.white(),
                check.actual,
                status_label,
                action_label,
            ));
        }
    }

    out.push_str(&format!(
        "{}\n",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(severity: Severity, issue_type: &str) -> ReviewIssue {
        ReviewIssue::new("test.rs", Some(1), severity, "test issue")
            .with_type(issue_type.to_string())
    }

    fn default_config() -> QualityGateConfig {
        QualityGateConfig::default()
    }

    #[test]
    fn gate_passes_on_no_issues() {
        let result = evaluate(&[], &default_config());
        assert_eq!(result.status, GateStatus::Pass);
        assert_eq!(result.total_findings, 0);
    }

    #[test]
    fn gate_fails_on_critical_with_default_config() {
        let issues = vec![make_issue(Severity::Critical, "security")];
        let config = QualityGateConfig {
            enabled: true,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        assert_eq!(result.status, GateStatus::Fail);
    }

    #[test]
    fn gate_passes_when_under_threshold() {
        let issues = vec![make_issue(Severity::Major, "bug")];
        let config = QualityGateConfig {
            enabled: true,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        // Default: max_major = usize::MAX, so 1 major passes
        assert_eq!(result.status, GateStatus::Pass);
    }

    #[test]
    fn gate_fails_on_security_findings() {
        let issues = vec![make_issue(Severity::Minor, "security")];
        let config = QualityGateConfig {
            enabled: true,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        // Default: max_security = 0
        assert_eq!(result.status, GateStatus::Fail);
    }

    #[test]
    fn gate_category_block_fails() {
        let issues = vec![make_issue(Severity::Major, "performance")];
        let mut categories = HashMap::new();
        categories.insert(
            "performance".to_string(),
            CategoryConfig {
                action: CategoryAction::Block,
                max_findings: 0,
            },
        );
        let config = QualityGateConfig {
            enabled: true,
            categories,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        assert_eq!(result.status, GateStatus::Fail);
    }

    #[test]
    fn gate_category_warn_does_not_fail() {
        let issues = vec![make_issue(Severity::Major, "performance")];
        let mut categories = HashMap::new();
        categories.insert(
            "performance".to_string(),
            CategoryConfig {
                action: CategoryAction::Warn,
                max_findings: 0,
            },
        );
        let config = QualityGateConfig {
            enabled: true,
            categories,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        // "warn" categories don't cause gate failure
        assert_eq!(result.status, GateStatus::Pass);
    }

    #[test]
    fn gate_category_ignore_is_skipped() {
        let issues = vec![make_issue(Severity::Critical, "style")];
        let mut categories = HashMap::new();
        categories.insert(
            "style".to_string(),
            CategoryConfig {
                action: CategoryAction::Ignore,
                max_findings: 0,
            },
        );
        let config = QualityGateConfig {
            enabled: true,
            categories,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        // "style" is ignored, but critical still fails (max_critical = 0)
        assert_eq!(result.status, GateStatus::Fail);
        // The category check should not appear in results
        assert!(!result.checks.iter().any(|c| c.name == "category:style"));
    }

    #[test]
    fn severity_counts_correct() {
        let issues = vec![
            make_issue(Severity::Critical, "bug"),
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Major, "performance"),
            make_issue(Severity::Minor, "style"),
            make_issue(Severity::Info, "suggestion"),
        ];
        let result = evaluate(&issues, &default_config());
        assert_eq!(result.severity_counts.critical, 2);
        assert_eq!(result.severity_counts.major, 1);
        assert_eq!(result.severity_counts.minor, 1);
        assert_eq!(result.severity_counts.info, 1);
        assert_eq!(result.total_findings, 5);
    }

    #[test]
    fn category_counts_case_insensitive() {
        let issues = vec![
            make_issue(Severity::Major, "Security"),
            make_issue(Severity::Major, "security"),
        ];
        let result = evaluate(&issues, &default_config());
        assert_eq!(result.category_counts.get("security"), Some(&2));
    }

    #[test]
    fn format_output_contains_status() {
        let issues = vec![make_issue(Severity::Critical, "bug")];
        let config = QualityGateConfig {
            enabled: true,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        let output = format_gate_output(&result);
        assert!(output.contains("FAILED"));
        assert!(output.contains("QUALITY GATE"));
    }

    #[test]
    fn format_output_pass() {
        let result = evaluate(&[], &default_config());
        let output = format_gate_output(&result);
        assert!(output.contains("PASSED"));
    }

    #[test]
    fn custom_thresholds() {
        let issues = vec![
            make_issue(Severity::Major, "bug"),
            make_issue(Severity::Major, "bug"),
        ];
        let config = QualityGateConfig {
            enabled: true,
            thresholds: ThresholdConfig {
                max_critical: 0,
                max_major: 1,
                max_minor: usize::MAX,
                max_security: 0,
            },
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        // 2 major > max_major (1) = FAIL
        assert_eq!(result.status, GateStatus::Fail);
        // Find the max_major check
        let major_check = result
            .checks
            .iter()
            .find(|c| c.name == "max_major")
            .unwrap();
        assert!(!major_check.passed);
        assert_eq!(major_check.actual, 2);
    }

    // ─── #58: disabled gate must never fail ───

    #[test]
    fn gate_disabled_never_fails_even_with_critical() {
        let issues = vec![
            make_issue(Severity::Critical, "security"),
            make_issue(Severity::Critical, "bug"),
        ];
        let config = QualityGateConfig {
            enabled: false,
            ..Default::default()
        };
        let result = evaluate(&issues, &config);
        assert_eq!(result.status, GateStatus::Pass);
        // No threshold checks are produced when disabled.
        assert!(!result.checks.is_empty());
        // total_findings still reflects input for reporting.
        assert_eq!(result.total_findings, 2);
        // Counts are still computed for reporting even when the gate is off.
        assert_eq!(result.severity_counts.critical, 2);
    }

    // ─── #57: CategoryAction case-insensitive enum ───

    #[test]
    fn category_action_deserializes_case_insensitive() {
        let yaml = "action: BLOCK\nmax_findings: 0";
        let cc: CategoryConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(cc.action, CategoryAction::Block);

        let yaml2 = "action: Warn";
        let cc2: CategoryConfig = serde_yaml_ng::from_str(yaml2).unwrap();
        assert_eq!(cc2.action, CategoryAction::Warn);
    }

    #[test]
    fn category_action_unknown_value_errors() {
        let yaml = "action: blok\nmax_findings: 0";
        let result: Result<CategoryConfig, _> = serde_yaml_ng::from_str(yaml);
        // A typo must fail loudly, not silently become blocking.
        assert!(result.is_err());
    }
}
