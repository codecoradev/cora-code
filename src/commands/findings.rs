//! `cora findings` subcommand — manage review findings stored in cora.db.

use anyhow::Result;
use colored::Colorize;

use crate::engine::review_store::{self, FindingFilter, FindingStats, ReviewStore, Transition};

/// Exit codes.
const EXIT_OK: i32 = 0;
const EXIT_NOT_FOUND: i32 = 1;

/// Sub-actions for `cora findings`.
#[derive(Debug, clap::Subcommand)]
pub enum FindingsAction {
    /// List findings (default: open only)
    List {
        /// Show all findings including resolved/dismissed
        #[clap(long)]
        all: bool,

        /// Filter by severity (info, minor, major, critical)
        #[clap(long)]
        severity: Option<String>,

        /// Filter by file path substring
        #[clap(long)]
        file: Option<String>,

        /// Output as JSON
        #[clap(long)]
        json: bool,

        /// Maximum number of findings to show
        #[clap(long, default_value = "50")]
        limit: usize,
    },

    /// Show summary statistics
    Stats {
        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Dismiss a finding (mark as won't-fix)
    Dismiss {
        /// Finding ID to dismiss
        id: i64,

        /// Optional reason for dismissal
        #[clap(long)]
        reason: Option<String>,
    },

    /// Reopen a resolved or dismissed finding
    Reopen {
        /// Finding ID to reopen
        id: i64,
    },
}

/// Execute the `cora findings` subcommand.
pub fn execute_findings(action: &FindingsAction) -> Result<i32> {
    // Read-only actions use a read-only connection.
    // Write actions (dismiss, reopen) use a read-write connection.
    match action {
        FindingsAction::List { .. } | FindingsAction::Stats { .. } => {
            let Ok(conn) = review_store::open_read() else {
                eprintln!("{}", "Error: could not open cora.db".red());
                return Ok(EXIT_NOT_FOUND);
            };
            let store = ReviewStore::new(&conn);
            match action {
                FindingsAction::List {
                    all,
                    severity,
                    file,
                    json,
                    limit,
                } => {
                    let filter = FindingFilter {
                        all: *all,
                        severity: severity.clone(),
                        file: file.clone(),
                        limit: *limit,
                    };
                    list_findings(&store, &filter, *json)
                }
                FindingsAction::Stats { json } => stats(&store, *json),
                _ => unreachable!(),
            }
        }
        FindingsAction::Dismiss { id, reason } => {
            let Ok(conn) = review_store::open_write() else {
                eprintln!("{}", "Error: could not open cora.db for writing".red());
                return Ok(EXIT_NOT_FOUND);
            };
            dismiss(&ReviewStore::new(&conn), *id, reason.as_deref())
        }
        FindingsAction::Reopen { id } => {
            let Ok(conn) = review_store::open_write() else {
                eprintln!("{}", "Error: could not open cora.db for writing".red());
                return Ok(EXIT_NOT_FOUND);
            };
            reopen(&ReviewStore::new(&conn), *id)
        }
    }
}

fn list_findings(store: &ReviewStore<'_>, filter: &FindingFilter, json: bool) -> Result<i32> {
    let rows = store.list_findings(filter)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(EXIT_OK);
    }

    if rows.is_empty() {
        println!("{}", "No findings found.".dimmed());
        return Ok(EXIT_OK);
    }

    println!(
        "{} {} finding(s)\n",
        "▸".cyan(),
        rows.len().to_string().bold()
    );
    for r in &rows {
        let sev = match r.severity.as_str() {
            "CRITICAL" => r.severity.clone().red().to_string(),
            "MAJOR" => r.severity.clone().yellow().to_string(),
            "MINOR" => r.severity.clone().green().to_string(),
            _ => r.severity.clone().dimmed().to_string(),
        };
        let status_tag = match r.status.as_str() {
            "open" => "OPEN".green().to_string(),
            "resolved" => "RESOLVED".dimmed().to_string(),
            "dismissed" => "DISMISSED".dimmed().to_string(),
            _ => r.status.to_uppercase().dimmed().to_string(),
        };
        let line_info = match r.line_number {
            Some(l) => format!(":{}", l),
            None => String::new(),
        };
        println!(
            "  #{} {} {} {}{} [{}]",
            r.id.to_string().dimmed(),
            sev,
            r.file_path.to_string().blue(),
            line_info.dimmed(),
            format_args!(" | {}", r.title),
            status_tag,
        );
    }

    Ok(EXIT_OK)
}

fn stats(store: &ReviewStore<'_>, json: bool) -> Result<i32> {
    let FindingStats {
        total,
        open,
        resolved,
        dismissed,
        reviews,
    } = store.stats()?;

    if json {
        let stats = serde_json::json!({
            "total_findings": total,
            "open": open,
            "resolved": resolved,
            "dismissed": dismissed,
            "total_reviews": reviews,
            "resolution_rate": if total > 0 { (resolved as f64 / total as f64 * 100.0).round() } else { 0.0 },
        });
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(EXIT_OK);
    }

    println!("{}", "Findings Summary".bold());
    println!();
    println!("  Reviews:      {}", reviews.to_string().bold());
    println!("  Total:        {}", total);
    println!("  {}", format!("Open:         {}", open).green());
    println!("  {}", format!("Resolved:     {}", resolved).dimmed());
    println!("  {}", format!("Dismissed:    {}", dismissed).dimmed());
    if total > 0 {
        let rate = resolved as f64 / total as f64 * 100.0;
        println!("  Resolution:   {:.1}%", rate);
    }

    Ok(EXIT_OK)
}

fn dismiss(store: &ReviewStore<'_>, id: i64, reason: Option<&str>) -> Result<i32> {
    match store.dismiss(id, reason)? {
        Transition::NotFound => {
            eprintln!("{}", format!("Finding #{} not found.", id).red());
            Ok(EXIT_NOT_FOUND)
        }
        _ => {
            println!("{} Finding #{} dismissed.", "✓".green(), id);
            Ok(EXIT_OK)
        }
    }
}

fn reopen(store: &ReviewStore<'_>, id: i64) -> Result<i32> {
    match store.reopen(id)? {
        Transition::Unchanged => {
            println!("{}", format!("Finding #{} is already open.", id).yellow());
            Ok(EXIT_OK)
        }
        Transition::NotFound => {
            eprintln!("{}", format!("Finding #{} not found.", id).red());
            Ok(EXIT_NOT_FOUND)
        }
        Transition::Applied => {
            println!("{} Finding #{} reopened.", "✓".green(), id);
            Ok(EXIT_OK)
        }
    }
}
