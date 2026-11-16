//! `cora routes` — list HTTP endpoints detected as ROUTE edges in the code graph.
//!
//! Route detection lives in [`crate::index::extract::detect_routes`] (Axum,
//! Actix-web, Go net/http/chi/gin, Express/Fastify, Flask/FastAPI). Detected
//! routes are stored as edges with `kind = 'ROUTE'`, where:
//! - `source` = handler function name (or `route_line_<n>` fallback)
//! - `target` = `METHOD /path` (e.g. `GET /api/users`)
//!
//! This command lists those edges with optional `--method` / `--prefix` filters.

use rusqlite::Connection;
use serde::Serialize;

/// A detected HTTP route (one row from the `edges` table where `kind = 'ROUTE'`).
#[derive(Debug, Serialize)]
pub struct Route {
    /// HTTP method (e.g. `GET`, `POST`). Empty when the framework didn't expose it.
    pub method: String,
    /// Route path (e.g. `/api/users`).
    pub path: String,
    /// Handler function name (or `route_line_<n>` fallback).
    pub handler: String,
    /// Source file where the route is registered.
    pub file: String,
    /// Line number of the registration.
    pub line: u32,
}

/// Route frameworks the detector understands, keyed by the indexed language.
const ROUTE_FRAMEWORKS: &[(&[&str], &[&str])] = &[
    (&["rs"], &["Axum", "Actix-web"]),
    (&["go"], &["net/http", "chi/gin"]),
    (&["ts", "tsx", "javascript", "jsx"], &["Express", "Fastify"]),
    (&["python"], &["Flask", "FastAPI"]),
];

/// What `cora routes` looked at: the frameworks it can detect in the
/// languages present in the project, and how many indexed files it scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteScan {
    pub frameworks: Vec<String>,
    pub files_scanned: usize,
}

/// Summarise which frameworks were searched and how many files that covered.
pub fn route_scan(conn: &Connection, project_id: i64) -> anyhow::Result<RouteScan> {
    let mut stmt = conn
        .prepare("SELECT language, COUNT(*) FROM files WHERE project_id = ?1 GROUP BY language")?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    let per_language: Vec<(String, usize)> = rows.collect::<Result<_, _>>()?;

    let mut scan = RouteScan {
        frameworks: Vec::new(),
        files_scanned: 0,
    };
    for (languages, frameworks) in ROUTE_FRAMEWORKS {
        let files: usize = per_language
            .iter()
            .filter(|(lang, _)| languages.contains(&lang.as_str()))
            .map(|(_, n)| n)
            .sum();
        if files > 0 {
            scan.files_scanned += files;
            scan.frameworks
                .extend(frameworks.iter().map(|f| f.to_string()));
        }
    }
    Ok(scan)
}

/// One-line description of a [`RouteScan`] for the text output.
fn describe_scan(scan: &RouteScan) -> String {
    if scan.frameworks.is_empty() {
        "Searched 0 files: no indexed file is in a language with route detection \
         (Rust, Go, TypeScript/JavaScript, Python)."
            .to_string()
    } else {
        format!(
            "Searched {} files for: {}.",
            scan.files_scanned,
            scan.frameworks.join(", ")
        )
    }
}

/// Query ROUTE edges for a project, with optional filters.
///
/// - `method_filter`: case-insensitive method match (e.g. `GET`). Empty = all.
/// - `prefix_filter`: only paths starting with this prefix (e.g. `/api`).
pub fn list_routes(
    conn: &Connection,
    project_id: i64,
    method_filter: Option<&str>,
    prefix_filter: Option<&str>,
) -> anyhow::Result<Vec<Route>> {
    // The `target` column holds either `METHOD /path` or just `/path`. Split it
    // client-side so we can filter on method and path independently and keep the
    // query simple (SQLite doesn't need custom functions).
    let mut stmt = conn.prepare(
        "SELECT source, target, file, line FROM edges
         WHERE project_id = ?1 AND kind = 'ROUTE'
         ORDER BY target",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        let source: String = row.get(0)?;
        let target: String = row.get(1)?;
        let file: String = row.get(2)?;
        let line: i64 = row.get(3)?;
        Ok((source, target, file, line as u32))
    })?;

    let mut routes = Vec::new();
    for row in rows {
        let (handler, target, file, line) = row?;
        let (method, path) = split_target(&target);

        if let Some(m) = method_filter {
            if !m.is_empty() && !method.eq_ignore_ascii_case(m) {
                continue;
            }
        }
        if let Some(p) = prefix_filter {
            if !path.starts_with(p) {
                continue;
            }
        }

        routes.push(Route {
            method,
            path,
            handler,
            file,
            line,
        });
    }
    Ok(routes)
}

/// Split a `target` (`METHOD /path` or `/path`) into `(method, path)`.
fn split_target(target: &str) -> (String, String) {
    // Targets are either "GET /api/users" or "/api/users".
    if let Some((method, path)) = target.split_once(' ') {
        // Guard against paths containing spaces (rare) — only treat the first
        // token as a method if it looks like one (all-letters).
        if method.chars().all(|c| c.is_ascii_alphabetic()) {
            return (method.to_string(), path.to_string());
        }
    }
    (String::new(), target.to_string())
}

/// CLI entry point for `cora routes`.
pub fn execute_routes_cli(
    method: Option<&str>,
    prefix: Option<&str>,
    json_flag: bool,
) -> anyhow::Result<String> {
    let (conn, project_id, _root) =
        crate::engine::index_bridge::IndexBridge::open_or_create_cwd()?.into_strict_parts()?;

    let routes = list_routes(&conn, project_id, method, prefix)?;

    if json_flag {
        Ok(serde_json::to_string_pretty(&routes)?)
    } else {
        let scan = route_scan(&conn, project_id)?;
        format_routes(&routes, method, prefix, &scan)
    }
}

/// Format routes as human-readable text.
fn format_routes(
    routes: &[Route],
    method: Option<&str>,
    prefix: Option<&str>,
    scan: &RouteScan,
) -> anyhow::Result<String> {
    if routes.is_empty() {
        let mut msg = "No routes detected. Run `cora index` first.".to_string();
        if let Some(m) = method {
            msg.push_str(&format!(" (filtered by method={})", m));
        }
        if let Some(p) = prefix {
            msg.push_str(&format!(" (filtered by prefix={})", p));
        }
        msg.push('\n');
        msg.push_str(&describe_scan(scan));
        return Ok(msg);
    }

    let mut lines = Vec::new();
    lines.push(format!("Detected HTTP routes ({})", routes.len()));
    lines.push(describe_scan(scan));
    lines.push("──────────────────────────────────────────────────────".to_string());

    // Column widths for alignment
    let method_w = routes
        .iter()
        .map(|r| r.method.len())
        .max()
        .unwrap_or(0)
        .max(6);
    for r in routes {
        let m = if r.method.is_empty() {
            "ANY".to_string()
        } else {
            r.method.clone()
        };
        lines.push(format!(
            "  {:<method_w$}  {:<40}  {} → {}:{}",
            m,
            r.path,
            r.handler,
            r.file,
            r.line,
            method_w = method_w
        ));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_with_method() {
        let (m, p) = split_target("GET /api/users");
        assert_eq!(m, "GET");
        assert_eq!(p, "/api/users");
    }

    #[test]
    fn split_without_method() {
        let (m, p) = split_target("/api/users");
        assert_eq!(m, "");
        assert_eq!(p, "/api/users");
    }

    #[test]
    fn split_path_with_space_not_treated_as_method() {
        // A path like "/save my file" shouldn't be mis-split.
        let (m, p) = split_target("/save my file");
        assert_eq!(m, "");
        assert_eq!(p, "/save my file");
    }

    #[test]
    fn split_lowercase_method_still_splits() {
        let (m, _p) = split_target("post /login");
        assert_eq!(m, "post");
    }

    fn mem_conn() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/routes-test").unwrap();
        (conn, pid)
    }

    fn add_file(conn: &Connection, pid: i64, path: &str, language: &str) {
        conn.execute(
            "INSERT INTO files (path, fingerprint, last_indexed, language, project_id)
             VALUES (?1, 'fp', 'now', ?2, ?3)",
            rusqlite::params![path, language, pid],
        )
        .unwrap();
    }

    #[test]
    fn route_scan_lists_frameworks_and_counts_files() {
        let (conn, pid) = mem_conn();
        add_file(&conn, pid, "src/a.rs", "rs");
        add_file(&conn, pid, "src/b.rs", "rs");
        add_file(&conn, pid, "web/app.ts", "ts");
        add_file(&conn, pid, "README.md", "markdown");
        let scan = route_scan(&conn, pid).unwrap();
        assert_eq!(scan.files_scanned, 3);
        assert_eq!(
            scan.frameworks,
            vec!["Axum", "Actix-web", "Express", "Fastify"]
        );
    }

    #[test]
    fn empty_result_names_what_was_searched() {
        let scan = RouteScan {
            frameworks: vec!["Axum".into(), "Actix-web".into()],
            files_scanned: 41,
        };
        let msg = format_routes(&[], None, None, &scan).unwrap();
        assert!(msg.contains("No routes detected"));
        assert!(msg.contains("Searched 41 files for: Axum, Actix-web."));
    }

    #[test]
    fn route_scan_with_no_supported_language() {
        let (conn, pid) = mem_conn();
        add_file(&conn, pid, "README.md", "markdown");
        let scan = route_scan(&conn, pid).unwrap();
        assert_eq!(scan.files_scanned, 0);
        assert!(describe_scan(&scan).starts_with("Searched 0 files"));
    }
}
