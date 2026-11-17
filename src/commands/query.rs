//! Simplified graph query DSL for the code graph.
//!
//! Parse simple patterns like:
//! - `"main -> *"` — all symbols called by main (outgoing CALLS)
//! - `"* -> main"` — all symbols that call main (incoming CALLERS)
//! - `"AuthService -> *"` — all symbols called by AuthService
//! - `"MyStruct"` — symbol info lookup

use crate::index::std_names::is_std_method;
use rusqlite::Connection;
use std::collections::HashMap;

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

/// Upper bound on rows fetched before std names are filtered out and the
/// result is cut to `limit`.
const STD_FILTER_FETCH_CAP: usize = 10_000;

/// Should `name` be hidden from results? Only std-looking names that the
/// project does not define itself, and that the pattern did not ask for.
fn is_hidden_std(
    conn: &Connection,
    project_id: i64,
    pattern: &QueryPattern,
    name: &str,
    defined_cache: &mut HashMap<String, bool>,
) -> bool {
    if !is_std_method(name) || name == pattern.source || name == pattern.target {
        return false;
    }
    let defined = *defined_cache.entry(name.to_string()).or_insert_with(|| {
        conn.query_row(
            "SELECT 1 FROM symbols WHERE project_id = ?1 AND name = ?2 LIMIT 1",
            rusqlite::params![project_id, name],
            |_| Ok(()),
        )
        .is_ok()
    });
    !defined
}

/// Rows of a graph query plus how many std/core names were left out.
#[derive(Debug, Clone, Default)]
pub struct QueryResults {
    pub items: Vec<serde_json::Value>,
    /// Number of result rows hidden because they are std/core method names.
    pub hidden_std: usize,
}

/// Execute a parsed query against the symbol index.
///
/// Each item is a JSON object `{symbol, kind, file, line}`. Unless
/// `include_std` is set, std/core method names (see [`crate::index::std_names`]) the
/// project does not define are dropped before `limit` is applied.
pub fn execute_query(
    pattern: &QueryPattern,
    project_id: i64,
    conn: &Connection,
    limit: usize,
    include_std: bool,
) -> anyhow::Result<QueryResults> {
    let fetch_limit = if include_std {
        limit
    } else {
        limit.max(STD_FILTER_FETCH_CAP)
    };
    let fetched = match pattern.direction {
        QueryDirection::Outgoing => execute_outgoing(pattern, project_id, conn, fetch_limit)?,
        QueryDirection::Incoming => execute_incoming(pattern, project_id, conn, fetch_limit)?,
    };

    let mut out = QueryResults::default();
    let mut defined_cache = HashMap::new();
    for item in fetched {
        let name = item.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        if !include_std && is_hidden_std(conn, project_id, pattern, name, &mut defined_cache) {
            out.hidden_std += 1;
        } else if out.items.len() < limit {
            out.items.push(item);
        }
    }
    Ok(out)
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
        let callees = crate::index::graph::find_callees(conn, project_id, &pattern.source, limit)?;
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
        let callees = crate::index::graph::find_callees(conn, project_id, &pattern.source, limit)?;
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
        let callers = crate::index::graph::find_callers(conn, project_id, &pattern.source, limit)?;
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
        let callers = crate::index::graph::find_callers(conn, project_id, &pattern.source, limit)?;
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
    include_std: bool,
) -> anyhow::Result<String> {
    let (conn, project_id, _root) =
        crate::engine::index_bridge::IndexBridge::open_or_create_cwd()?.into_strict_parts()?;

    let pattern = parse_query(pattern_str)?;
    let results = execute_query(&pattern, project_id, &conn, limit, include_std)?;

    if json_flag {
        Ok(serde_json::to_string_pretty(&results.items)?)
    } else {
        format_results(&pattern, &results)
    }
}

/// Format query results as human-readable text.
fn format_results(pattern: &QueryPattern, results: &QueryResults) -> anyhow::Result<String> {
    let arr = &results.items;
    let hidden_note = if results.hidden_std > 0 {
        format!(
            "{} std/core method calls hidden (use --include-std to show)",
            results.hidden_std
        )
    } else {
        String::new()
    };

    if arr.is_empty() {
        let mut msg = format!(
            "No results found for pattern: {}",
            describe_pattern(pattern)
        );
        if !hidden_note.is_empty() {
            msg.push_str(&format!("\n{hidden_note}"));
        }
        return Ok(msg);
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "Graph query: {} ({})",
        describe_pattern(pattern),
        arr.len()
    ));
    lines.push("─────────────────────────────────────".to_string());

    for item in arr {
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
    if !hidden_note.is_empty() {
        lines.push(format!("  ({hidden_note})"));
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

    fn mem_conn() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/query-test").unwrap();
        (conn, pid)
    }

    fn edge(caller: &str, callee: &str, line: u32) -> crate::index::graph::CallEdge {
        crate::index::graph::CallEdge {
            caller: caller.into(),
            callee: callee.into(),
            file: "src/main.rs".into(),
            line,
        }
    }

    fn symbols_of(r: &QueryResults) -> Vec<&str> {
        r.items
            .iter()
            .map(|v| v["symbol"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn std_methods_hidden_by_default_and_shown_with_include_std() {
        let (conn, pid) = mem_conn();
        crate::index::graph::store_edges(
            &conn,
            &[
                edge("main", "unwrap", 1),
                edge("main", "clone", 2),
                edge("main", "run_server", 3),
            ],
            pid,
        )
        .unwrap();
        let pat = parse_query("main -> *").unwrap();

        let hidden = execute_query(&pat, pid, &conn, 50, false).unwrap();
        assert_eq!(symbols_of(&hidden), vec!["run_server"]);
        assert_eq!(hidden.hidden_std, 2);

        let all = execute_query(&pat, pid, &conn, 50, true).unwrap();
        assert_eq!(all.items.len(), 3);
        assert_eq!(all.hidden_std, 0);
    }

    #[test]
    fn project_defined_std_name_is_not_hidden() {
        let (conn, pid) = mem_conn();
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, project_id)
             VALUES ('new', 'function', 'src/a.rs', 1, ?1)",
            rusqlite::params![pid],
        )
        .unwrap();
        crate::index::graph::store_edges(
            &conn,
            &[edge("main", "new", 1), edge("main", "unwrap", 2)],
            pid,
        )
        .unwrap();
        let pat = parse_query("main -> *").unwrap();
        let r = execute_query(&pat, pid, &conn, 50, false).unwrap();
        assert_eq!(symbols_of(&r), vec!["new"]);
        assert_eq!(r.hidden_std, 1);
    }

    #[test]
    fn std_filter_runs_before_limit() {
        let (conn, pid) = mem_conn();
        let mut edges: Vec<_> = (0..5).map(|i| edge("main", "unwrap", i)).collect();
        edges.push(edge("main", "real_work", 10));
        crate::index::graph::store_edges(&conn, &edges, pid).unwrap();
        let pat = parse_query("main -> *").unwrap();
        // With limit 1 the std rows must not crowd out the real callee.
        let r = execute_query(&pat, pid, &conn, 1, false).unwrap();
        assert_eq!(symbols_of(&r), vec!["real_work"]);
    }

    #[test]
    fn explicitly_queried_std_name_is_kept() {
        let (conn, pid) = mem_conn();
        crate::index::graph::store_edges(&conn, &[edge("main", "unwrap", 1)], pid).unwrap();
        let pat = parse_query("main -> unwrap").unwrap();
        let r = execute_query(&pat, pid, &conn, 50, false).unwrap();
        assert_eq!(symbols_of(&r), vec!["unwrap"]);
    }
}
