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
    let conn = crate::index::open_global_index()?;
    let (project_id, _root) = crate::index::resolve_project_id(&conn)?;

    let routes = list_routes(&conn, project_id, method, prefix)?;

    if json_flag {
        Ok(serde_json::to_string_pretty(&routes)?)
    } else {
        format_routes(&routes, method, prefix)
    }
}

/// Format routes as human-readable text.
fn format_routes(
    routes: &[Route],
    method: Option<&str>,
    prefix: Option<&str>,
) -> anyhow::Result<String> {
    if routes.is_empty() {
        let mut msg = "No routes detected. Run `cora index` first.".to_string();
        if let Some(m) = method {
            msg.push_str(&format!(" (filtered by method={})", m));
        }
        if let Some(p) = prefix {
            msg.push_str(&format!(" (filtered by prefix={})", p));
        }
        return Ok(msg);
    }

    let mut lines = Vec::new();
    lines.push(format!("Detected HTTP routes ({})", routes.len()));
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
}
