//! `cora findings` subcommand — manage review findings stored in cora.db.

use anyhow::Result;
use colored::Colorize;

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
            let conn = match crate::engine::db_writer::open_db_for_read() {
                Some(c) => c,
                None => {
                    eprintln!("{}", "Error: could not open cora.db".red());
                    return Ok(EXIT_NOT_FOUND);
                }
            };
            match action {
                FindingsAction::List {
                    all,
                    severity,
                    file,
                    json,
                    limit,
                } => list_findings(&conn, *all, severity, file, *json, *limit),
                FindingsAction::Stats { json } => stats(&conn, *json),
                _ => unreachable!(),
            }
        }
        FindingsAction::Dismiss { id, reason } => {
            let conn = match crate::engine::db_writer::open_db_for_write() {
                Some(c) => c,
                None => {
                    eprintln!("{}", "Error: could not open cora.db for writing".red());
                    return Ok(EXIT_NOT_FOUND);
                }
            };
            dismiss(&conn, *id, reason)
        }
        FindingsAction::Reopen { id } => {
            let conn = match crate::engine::db_writer::open_db_for_write() {
                Some(c) => c,
                None => {
                    eprintln!("{}", "Error: could not open cora.db for writing".red());
                    return Ok(EXIT_NOT_FOUND);
                }
            };
            reopen(&conn, *id)
        }
    }
}

fn list_findings(
    conn: &rusqlite::Connection,
    all: bool,
    severity: &Option<String>,
    file: &Option<String>,
    json: bool,
    limit: usize,
) -> Result<i32> {
    let mut sql = String::from(
        "SELECT f.id, f.severity, f.file_path, f.line_number, f.title, f.status,
               f.fingerprint, r.created_at
        FROM findings f
        JOIN reviews r ON f.review_id = r.id",
    );

    let mut wheres: Vec<String> = Vec::new();
    if !all {
        wheres.push("f.status = 'open'".to_string());
    }
    if let Some(s) = severity {
        wheres.push(format!("f.severity = '{}'", s.to_uppercase()));
    }
    if let Some(f) = file {
        wheres.push(format!("f.file_path LIKE '%{}%'", f.replace('"', "'")));
    }

    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    sql.push_str(" ORDER BY f.id DESC");
    sql.push_str(&format!(" LIMIT {}", limit));

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<ListRow> = stmt
        .query([])?
        .mapped(|r| {
            Ok(ListRow {
                id: r.get(0)?,
                severity: r.get(1)?,
                file_path: r.get(2)?,
                line_number: r.get(3)?,
                title: r.get(4)?,
                status: r.get(5)?,
                fingerprint: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .filter_map(|r| r.ok())
        .collect();

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

fn stats(conn: &rusqlite::Connection, json: bool) -> Result<i32> {
    let total: i64 = conn
        .query_row("SELECT count(*) FROM findings", [], |r| r.get(0))
        .unwrap_or(0);

    let open: i64 = conn
        .query_row(
            "SELECT count(*) FROM findings WHERE status = 'open'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let resolved: i64 = conn
        .query_row(
            "SELECT count(*) FROM findings WHERE status = 'resolved'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let dismissed: i64 = conn
        .query_row(
            "SELECT count(*) FROM findings WHERE status = 'dismissed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let reviews: i64 = conn
        .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
        .unwrap_or(0);

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

fn dismiss(conn: &rusqlite::Connection, id: i64, reason: &Option<String>) -> Result<i32> {
    let exists: bool = conn
        .query_row(
            "SELECT status FROM findings WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .is_ok();

    if !exists {
        eprintln!("{}", format!("Finding #{} not found.", id).red());
        return Ok(EXIT_NOT_FOUND);
    }

    conn.execute(
        "UPDATE findings SET status = 'dismissed' WHERE id = ?1",
        rusqlite::params![id],
    )?;

    let note = reason.as_deref().unwrap_or("Manually dismissed via CLI");
    conn.execute(
        "INSERT INTO finding_events (finding_id, event_type, note) VALUES (?1, 'dismissed', ?2)",
        rusqlite::params![id, note],
    )?;

    println!("{} Finding #{} dismissed.", "✓".green(), id);
    Ok(EXIT_OK)
}

fn reopen(conn: &rusqlite::Connection, id: i64) -> Result<i32> {
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM findings WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .ok();

    match status.as_deref() {
        Some("open") => {
            println!("{}", format!("Finding #{} is already open.", id).yellow());
            return Ok(EXIT_OK);
        }
        None => {
            eprintln!("{}", format!("Finding #{} not found.", id).red());
            return Ok(EXIT_NOT_FOUND);
        }
        _ => {}
    }

    conn.execute(
        "UPDATE findings SET status = 'open' WHERE id = ?1",
        rusqlite::params![id],
    )?;

    conn.execute(
        "INSERT INTO finding_events (finding_id, event_type, note) VALUES (?1, 'reopened', 'Manually reopened via CLI')",
        rusqlite::params![id],
    )?;

    println!("{} Finding #{} reopened.", "✓".green(), id);
    Ok(EXIT_OK)
}

#[derive(serde::Serialize)]
struct ListRow {
    id: i64,
    severity: String,
    file_path: String,
    line_number: Option<i64>,
    title: String,
    status: String,
    fingerprint: Option<String>,
    created_at: String,
}
