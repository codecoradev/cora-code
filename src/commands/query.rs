//! Simplified graph query DSL for the code graph.
//!
//! Parse simple patterns like:
//! - `"main -> *"` — all symbols called by main (outgoing CALLS)
//! - `"* -> main"` — all symbols that call main (incoming CALLERS)
//! - `"AuthService -> *"` — all symbols called by AuthService
//! - `"MyStruct"` — symbol info lookup

use rusqlite::Connection;

/// Parsed graph query pattern.
#[derive(Debug, Clone)]
pub struct QueryPattern {
    /// Symbol name or "*" for wildcard.
    pub source: String,
    /// Query direction.
    pub direction: QueryDirection,
    /// Target symbol name or "*" for wildcard.
    pub target: String,
}

/// Direction of the graph traversal.
#[derive(Debug, Clone, Copy)]
pub enum QueryDirection {
    /// source -> target (find callees)
    Outgoing,
    /// target -> source (find callers)
    Incoming,
}

/// Parse a graph query pattern string.
///
/// Supports:
/// - `"A -> B"` — outgoing (callees of A, optionally filtered to B)
/// - `"A <- B"` — incoming (callers of A, optionally filtered to B)
/// - `"A"` — symbol lookup (treated as outgoing with wildcard target)
pub fn parse_query(input: &str) -> anyhow::Result<QueryPattern> {
    let input = input.trim();

    if input.contains("->") {
        let parts: Vec<&str> = input.splitn(2, "->").collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid query pattern: expected 'SOURCE -> TARGET'");
        }
        Ok(QueryPattern {
            source: parts[0].trim().to_string(),
            direction: QueryDirection::Outgoing,
            target: parts[1].trim().to_string(),
        })
    } else if input.contains("<-") {
        let parts: Vec<&str> = input.splitn(2, "<-").collect();
        if parts.len() != 2 {
            anyhow::bail!("Invalid query pattern: expected 'TARGET <- SOURCE'");
        }
        // "A <- B" means "callers of A" where source=A, callers come from B
        Ok(QueryPattern {
            source: parts[1].trim().to_string(),
            direction: QueryDirection::Incoming,
            target: parts[0].trim().to_string(),
        })
    } else if input.is_empty() {
        anyhow::bail!("Empty query pattern");
    } else {
        // Just a symbol name — lookup with outgoing wildcard
        Ok(QueryPattern {
            source: input.to_string(),
            direction: QueryDirection::Outgoing,
            target: "*".to_string(),
        })
    }
}

/// Execute a parsed query against the symbol index.
///
/// Returns a JSON array of objects with `{symbol, kind, file, line}`.
pub fn execute_query(
    pattern: &QueryPattern,
    project_id: i64,
    conn: &Connection,
    limit: usize,
) -> anyhow::Result<serde_json::Value> {
    let results = match pattern.direction {
        QueryDirection::Outgoing => execute_outgoing(pattern, project_id, conn, limit)?,
        QueryDirection::Incoming => execute_incoming(pattern, project_id, conn, limit)?,
    };

    Ok(serde_json::json!(results))
}

fn execute_outgoing(
    pattern: &QueryPattern,
    project_id: i64,
    conn: &Connection,
    limit: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    if pattern.source == "*" {
        // List all top-level functions/procedures (entry points)
        let mut stmt = conn.prepare(
            "SELECT name, kind, file, line FROM symbols
             WHERE project_id = ?1 AND kind IN ('function', 'procedure')
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id, limit as i64], |row| {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let file: String = row.get(2)?;
            let line: i64 = row.get(3)?;
            Ok(serde_json::json!({
                "symbol": name,
                "kind": kind,
                "file": file,
                "line": line,
            }))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    } else if pattern.target == "*" {
        // Find all callees of source symbol
        let callees =
            crate::index::graph::find_callees(conn, project_id, &pattern.source, limit)?;
        Ok(callees
            .iter()
            .map(|c| {
                serde_json::json!({
                    "symbol": c.callee,
                    "kind": "",
                    "file": c.file,
                    "line": c.line,
                })
            })
            .collect())
    } else {
        // Find specific callee — filter results
        let callees =
            crate::index::graph::find_callees(conn, project_id, &pattern.source, limit)?;
        Ok(callees
            .iter()
            .filter(|c| c.callee == pattern.target)
            .map(|c| {
                serde_json::json!({
                    "symbol": c.callee,
                    "kind": "",
                    "file": c.file,
                    "line": c.line,
                })
            })
            .collect())
    }
}

fn execute_incoming(
    pattern: &QueryPattern,
    project_id: i64,
    conn: &Connection,
    limit: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    if pattern.source == "*" {
        // List all symbols that are called by something (i.e., have callers)
        let mut stmt = conn.prepare(
            "SELECT DISTINCT callee AS name, '' AS kind, file, line FROM call_graph
             WHERE project_id = ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![project_id, limit as i64], |row| {
            let name: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let file: String = row.get(2)?;
            let line: i64 = row.get(3)?;
            Ok(serde_json::json!({
                "symbol": name,
                "kind": kind,
                "file": file,
                "line": line,
            }))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    } else if pattern.target == "*" {
        // Find all callers of source symbol
        let callers =
            crate::index::graph::find_callers(conn, project_id, &pattern.source, limit)?;
        Ok(callers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "symbol": c.caller,
                    "kind": "",
                    "file": c.file,
                    "line": c.line,
                })
            })
            .collect())
    } else {
        // Find specific caller — filter results
        let callers =
            crate::index::graph::find_callers(conn, project_id, &pattern.source, limit)?;
        Ok(callers
            .iter()
            .filter(|c| c.caller == pattern.target)
            .map(|c| {
                serde_json::json!({
                    "symbol": c.caller,
                    "kind": "",
                    "file": c.file,
                    "line": c.line,
                })
            })
            .collect())
    }
}

/// CLI entry point for `cora query`.
///
/// Opens the global index, resolves project, parses pattern, executes, and formats output.
pub fn execute_query_cli(
    pattern_str: &str,
    json_flag: bool,
    limit: usize,
) -> anyhow::Result<String> {
    let conn = crate::index::open_global_index()?;
    let (project_id, _root) = crate::index::resolve_project_id(&conn)?;

    let pattern = parse_query(pattern_str)?;
    let results = execute_query(&pattern, project_id, &conn, limit)?;

    if json_flag {
        Ok(serde_json::to_string_pretty(&results)?)
    } else {
        format_results(&pattern, &results)
    }
}

/// Format query results as human-readable text.
fn format_results(
    pattern: &QueryPattern,
    results: &serde_json::Value,
) -> anyhow::Result<String> {
    let arr = results
        .as_array()
        .cloned()
        .unwrap_or_default();

    if arr.is_empty() {
        return Ok(format!("No results found for pattern: {}", describe_pattern(pattern)));
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "Graph query: {} ({})",
        describe_pattern(pattern),
        arr.len()
    ));
    lines.push("─────────────────────────────────────".to_string());

    for item in &arr {
        let symbol = item.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("?");
        let line = item.get("line").and_then(|v| v.as_i64()).unwrap_or(0);

        if !kind.is_empty() {
            lines.push(format!("  {} {} {}:{}", kind, symbol, file, line));
        } else {
            lines.push(format!("  {} {}:{}", symbol, file, line));
        }
    }

    Ok(lines.join("\n"))
}

/// Describe the pattern for display.
fn describe_pattern(pattern: &QueryPattern) -> String {
    match pattern.direction {
        QueryDirection::Outgoing => {
            if pattern.source == "*" {
                "entry points (all top-level functions)".to_string()
            } else if pattern.target == "*" {
                format!("callees of {}", pattern.source)
            } else {
                format!("{} -> {}", pattern.source, pattern.target)
            }
        }
        QueryDirection::Incoming => {
            if pattern.source == "*" {
                "called symbols (have callers)".to_string()
            } else if pattern.target == "*" {
                format!("callers of {}", pattern.source)
            } else {
                format!("{} <- {}", pattern.source, pattern.target)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_outgoing_wildcard() {
        let p = parse_query("main -> *").unwrap();
        assert_eq!(p.source, "main");
        assert_eq!(p.target, "*");
        assert!(matches!(p.direction, QueryDirection::Outgoing));
    }

    #[test]
    fn parse_incoming_wildcard() {
        let p = parse_query("* -> main").unwrap();
        assert_eq!(p.source, "*");
        assert_eq!(p.target, "main");
        assert!(matches!(p.direction, QueryDirection::Outgoing));
    }

    #[test]
    fn parse_incoming_arrow() {
        let p = parse_query("main <- *").unwrap();
        assert_eq!(p.source, "*");
        assert_eq!(p.target, "main");
        assert!(matches!(p.direction, QueryDirection::Incoming));
    }

    #[test]
    fn parse_symbol_only() {
        let p = parse_query("MyStruct").unwrap();
        assert_eq!(p.source, "MyStruct");
        assert_eq!(p.target, "*");
        assert!(matches!(p.direction, QueryDirection::Outgoing));
    }

    #[test]
    fn parse_specific_target() {
        let p = parse_query("AuthService -> authenticate").unwrap();
        assert_eq!(p.source, "AuthService");
        assert_eq!(p.target, "authenticate");
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_query("").is_err());
        assert!(parse_query("  ").is_err());
    }
}
