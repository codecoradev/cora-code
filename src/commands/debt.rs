//! `cora debt` subcommand — show tech debt report from review history.

use anyhow::Result;
use chrono::DateTime;
use colored::Colorize;
use std::io::Write;

use crate::engine::debt_tracker;

/// Exit codes.
const EXIT_OK: i32 = 0;
const EXIT_NO_DATA: i32 = 1;

/// Debt subcommand options.
pub struct DebtOptions {
    /// Output as JSON.
    pub json: bool,
    /// Show ASCII trend graph.
    pub trend: bool,
    /// Filter: only snapshots since this git tag or date (YYYY-MM-DD).
    pub since: Option<String>,
    /// Filter: only snapshots for this branch.
    pub branch: Option<String>,
    /// Generate shields.io badge JSON.
    pub badge: bool,
    /// Show estimated debt fix time.
    pub estimate: bool,
    /// Config file path (from --config global flag).
    pub config_path: Option<String>,
}

/// Execute the `cora debt` subcommand.
pub fn execute_debt(opts: &DebtOptions) -> Result<i32> {
    let config = crate::config::loader::load_config(
        opts.config_path.as_deref(),
        None,
        None,
        None,
        None,
        false,
    )?;

    if !config.debt.enabled {
        println!(
            "{}",
            "Debt tracking is disabled. Enable it in .cora.yaml:".yellow()
        );
        println!("  debt:");
        println!("    enabled: true");
        return Ok(EXIT_OK);
    }

    // Try DB as primary SoT first (cora.db reviews table), fall back to file snapshots
    let cwd = std::env::current_dir().unwrap_or_default();
    let cwd_str = cwd.to_string_lossy().to_string();

    let mut snapshots = debt_tracker::load_snapshots_from_db(&cwd_str);

    if snapshots.is_empty() {
        // Fallback: legacy file-based snapshots
        snapshots = debt_tracker::load_snapshots(config.debt.history_dir.as_deref());
    }

    if snapshots.is_empty() {
        println!(
            "{}",
            "No review history found. Run `cora review` to build history.".yellow()
        );
        return Ok(EXIT_NO_DATA);
    }

    // Apply filters
    if let Some(ref since) = opts.since {
        snapshots = filter_since(&snapshots, since);
    }
    if let Some(ref branch) = opts.branch {
        snapshots.retain(|s| s.branch.as_deref() == Some(branch.as_str()));
    }

    if snapshots.is_empty() {
        println!("{}", "No snapshots match the given filters.".yellow());
        return Ok(EXIT_NO_DATA);
    }

    // Cleanup old snapshots (best-effort)
    let _ = debt_tracker::cleanup_old_snapshots(
        config.debt.history_dir.as_deref(),
        config.debt.retention_days,
    );

    let report = debt_tracker::aggregate(&snapshots);

    if opts.badge {
        let badge = if opts.estimate {
            debt_tracker::generate_badge_with_debt(&report)
        } else {
            debt_tracker::generate_badge(&report)
        };
        let json = serde_json::to_string(&badge)?;
        println!("{json}");
        return Ok(EXIT_OK);
    }

    if opts.json {
        // Add debt estimation to report output
        let hours = debt_tracker::estimate_debt_hours(&report.findings);
        let mut json_report = serde_json::to_value(&report)?;
        if let Some(obj) = json_report.as_object_mut() {
            obj.insert("estimated_debt_hours".to_string(), serde_json::json!(hours));
            obj.insert(
                "estimated_debt_human".to_string(),
                serde_json::json!(debt_tracker::format_debt_hours(hours)),
            );
        }
        let json = serde_json::to_string_pretty(&json_report)?;
        println!("{json}");
        return Ok(EXIT_OK);
    }

    if opts.trend {
        print_trend_graph(&snapshots);
        println!();
    }

    print_debt_table(&report, opts.estimate);

    Ok(EXIT_OK)
}

/// Filter snapshots since a date or git tag.
fn filter_since(
    snapshots: &[debt_tracker::DebtSnapshot],
    since: &str,
) -> Vec<debt_tracker::DebtSnapshot> {
    // Try parsing as date first (YYYY-MM-DD or full ISO 8601)
    if let Ok(date) = chrono::NaiveDate::parse_from_str(since, "%Y-%m-%d") {
        let cutoff = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        return snapshots
            .iter()
            .filter(|s| s.timestamp >= cutoff)
            .cloned()
            .collect();
    }

    // Try full ISO 8601 / RFC 3339
    if let Ok(date) = DateTime::parse_from_rfc3339(since) {
        let cutoff = date.with_timezone(&chrono::Utc);
        return snapshots
            .iter()
            .filter(|s| s.timestamp >= cutoff)
            .cloned()
            .collect();
    }

    // Try as git tag — resolve to commit date
    let tag_date = resolve_tag_date(since);
    if let Some(cutoff) = tag_date {
        return snapshots
            .iter()
            .filter(|s| s.timestamp >= cutoff)
            .cloned()
            .collect();
    }

    // Fallback: return all
    eprintln!(
        "{}",
        format!("Warning: could not resolve '{since}' as date or git tag. Showing all snapshots.")
            .yellow()
    );
    snapshots.to_vec()
}

/// Resolve a git tag to its commit timestamp.
fn resolve_tag_date(tag: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%cI", tag])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let date_str = String::from_utf8(output.stdout).ok()?;
    let date_str = date_str.trim();

    DateTime::parse_from_rfc3339(date_str)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
}

/// Print the debt report as a terminal table.
fn print_debt_table(report: &debt_tracker::DebtReport, show_estimate: bool) {
    let mut out = Vec::new();

    // Header
    writeln!(
        out,
        "\n{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    )
    .unwrap();
    writeln!(out, "  {}", "TECH DEBT REPORT".cyan().bold()).unwrap();
    writeln!(
        out,
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    )
    .unwrap();

    // Period
    let period = match (report.period_start, report.period_end) {
        (Some(s), Some(e)) => format!("{} to {}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d")),
        _ => "N/A".to_string(),
    };
    writeln!(out, "  Period:    {}", period.white()).unwrap();
    writeln!(
        out,
        "  Reviews:   {}",
        report.reviews_analyzed.to_string().white()
    )
    .unwrap();

    // Overall trend
    let trend_sym = trend_icon(&report.trend);
    let trend_colored = color_trend(&report.trend, &format!("{} {}", trend_sym, report.trend));
    writeln!(out, "  Trend:     {}", trend_colored).unwrap();

    // Quality score
    let score_colored = format_score(report.quality_score_avg);
    let score_change = format_change_f64(report.quality_score_change);
    writeln!(out, "  Quality:   {} {}", score_colored, score_change).unwrap();

    // Total findings
    let change_str = format_change_i64(report.change_from_previous);
    writeln!(
        out,
        "  Findings:  {} {}",
        report.total_findings.to_string().white(),
        change_str
    )
    .unwrap();

    // Debt estimation
    if show_estimate {
        let hours = debt_tracker::estimate_debt_hours(&report.findings);
        let human = debt_tracker::format_debt_hours(hours);
        writeln!(out, "  Est. Debt: {}", human.yellow()).unwrap();
    }

    // Severity breakdown
    writeln!(out).unwrap();
    writeln!(out, "  {}", "Severity Breakdown:".white().bold()).unwrap();
    for sev in &["critical", "major", "minor", "info"] {
        let count = report.findings.get(*sev).copied().unwrap_or(0);
        let label = match *sev {
            "critical" => "critical".red().to_string(),
            "major" => "major".yellow().to_string(),
            "minor" => "minor".blue().to_string(),
            _ => "info".to_string(),
        };
        writeln!(out, "    {:<12} {}", label, count).unwrap();
    }

    // Category breakdown
    if !report.categories.is_empty() {
        writeln!(out).unwrap();
        writeln!(
            out,
            "  {:<16} {:>8} {:>10} {}",
            "Category".white().bold(),
            "Total".white().bold(),
            "Change".white().bold(),
            "Trend".white().bold()
        )
        .unwrap();
        writeln!(
            out,
            "  {}",
            "─────────────────────────────────────────────────".dimmed()
        )
        .unwrap();

        for cat in &report.categories {
            let change = format_change_i64(cat.change);
            let trend = format!("{} {}", trend_icon(&cat.trend), cat.trend);
            let trend_colored = color_trend(&cat.trend, &trend);
            writeln!(
                out,
                "  {:<16} {:>8} {:>10} {}",
                cat.name, cat.count, change, trend_colored
            )
            .unwrap();
        }
    }

    writeln!(
        out,
        "{}",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".dimmed()
    )
    .unwrap();

    let output = String::from_utf8(out).unwrap_or_default();
    print!("{output}");
}

/// Print ASCII trend graph of quality scores over reviews.
fn print_trend_graph(snapshots: &[debt_tracker::DebtSnapshot]) {
    if snapshots.len() < 2 {
        println!(
            "\n{}",
            "Need at least 2 reviews to show a trend graph.".yellow()
        );
        return;
    }

    let scores: Vec<f64> = snapshots.iter().map(|s| s.quality_score).collect();
    let max_score = 10.0;
    let min_score = 0.0;

    // Graph dimensions
    let height = 8;
    let width = scores.len().min(40);

    let step = if scores.len() > width {
        scores.len() / width
    } else {
        1
    };

    let sampled: Vec<f64> = (0..width)
        .map(|i| {
            let idx = (i * step).min(scores.len() - 1);
            scores[idx]
        })
        .collect();

    println!("\n{}", "Quality Score Trend".cyan().bold());
    println!();

    for row in (0..height).rev() {
        let threshold = min_score + (max_score - min_score) * (row as f64 / (height - 1) as f64);
        let label = format!("{:.1}", threshold);
        let mut line = format!("{} ┤", label.dimmed());

        for (i, score) in sampled.iter().enumerate() {
            let bar_threshold_low =
                min_score + (max_score - min_score) * (row as f64 / (height - 1) as f64);
            let bar_threshold_high =
                min_score + (max_score - min_score) * ((row + 1) as f64 / (height - 1) as f64);

            // Score falls within this row's range
            // Special case: top row (row == height-1) should also catch max_score
            let in_range =
                *score >= bar_threshold_low && (*score < bar_threshold_high || row == height - 1);

            if in_range {
                line.push_str(&"●".green().to_string());
            } else if i > 0 {
                let prev = sampled[i - 1];
                if *score >= bar_threshold_low && prev < bar_threshold_low {
                    line.push_str(&"●".green().to_string());
                } else {
                    line.push_str("  ");
                }
            } else {
                line.push_str("  ");
            }
        }

        println!("{line}");
    }

    // X-axis
    print!("     └");
    for _ in 0..width {
        print!("──");
    }
    println!();

    print!("      ");
    // Show review indices
    let labels = ["R1"];
    for (i, l) in labels.iter().enumerate() {
        print!("{}", l.dimmed());
        if i < width - 1 {
            let gap = (width - 1).max(1);
            for _ in 0..(gap * 2 / 3).max(1) {
                print!(" ");
            }
        }
    }
    let last = format!("R{}", snapshots.len());
    println!(" {}", last.dimmed());
}

// ─── Formatting helpers ───

fn trend_icon(trend: &str) -> &'static str {
    match trend {
        "improving" => "▼",
        "worsening" => "▲",
        _ => "►",
    }
}

fn color_trend(trend: &str, text: &str) -> String {
    match trend {
        "improving" => text.green().to_string(),
        "worsening" => text.red().to_string(),
        _ => text.dimmed().to_string(),
    }
}

fn format_score(score: f64) -> String {
    if score >= 8.0 {
        format!("{:.1}/10", score).green().to_string()
    } else if score >= 5.0 {
        format!("{:.1}/10", score).yellow().to_string()
    } else {
        format!("{:.1}/10", score).red().to_string()
    }
}

fn format_change_i64(change: i64) -> String {
    if change > 0 {
        format!("(+{change})").red().to_string()
    } else if change < 0 {
        format!("({change})").green().to_string()
    } else {
        "(±0)".dimmed().to_string()
    }
}

fn format_change_f64(change: f64) -> String {
    if change > 0.1 {
        format!("(+{:.1})", change).green().to_string()
    } else if change < -0.1 {
        format!("({:.1})", change).red().to_string()
    } else {
        "(±0.0)".dimmed().to_string()
    }
}
