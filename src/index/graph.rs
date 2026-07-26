//! Call graph traversal for `cora callers` and `cora impact`.
//!
//! Uses the existing `engine/context/extraction.rs` symbol reference extraction
//! to build call edges at index time, then traverse them for queries.

use rusqlite::Connection;

#[allow(unused_imports)]
use super::symbols::{IndexedSymbol, SymbolKind};

/// A directed edge in the call graph: caller → callee.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct CallEdge {
    /// Symbol that makes the call.
    pub caller: String,
    /// Symbol that is called.
    pub callee: String,
    /// File where the call happens.
    pub file: String,
    /// Line number of the call site.
    pub line: u32,
}

/// Store call edges in the database, scoped to a project.
#[allow(dead_code)]
pub fn store_edges(
    conn: &Connection,
    edges: &[CallEdge],
    project_id: i64,
) -> anyhow::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut count = 0;
    for edge in edges {
        tx.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![edge.caller, edge.callee, edge.file, edge.line as i64, project_id],
        )?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

/// A typed edge in the knowledge graph.
#[cfg(feature = "tree-sitter")]
#[derive(Debug, Clone)]
pub struct KgEdge {
    /// Source symbol.
    pub source: String,
    /// Edge kind: CALLS, IMPORTS, IMPLEMENTS, INHERITS, CHILD_OF.
    pub kind: String,
    /// Target symbol.
    pub target: String,
    /// File where the relationship is defined.
    pub file: String,
    /// Line number.
    pub line: u32,
}

/// Store knowledge graph edges in the `edges` table.
#[cfg(feature = "tree-sitter")]
#[allow(dead_code)]
pub fn store_kg_edges(
    conn: &Connection,
    edges: &[KgEdge],
    project_id: i64,
) -> anyhow::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut count = 0;
    for edge in edges {
        tx.execute(
            "INSERT INTO edges (source, kind, target, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![edge.source, edge.kind, edge.target, edge.file, edge.line as i64, project_id],
        )?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

/// Clear knowledge graph edges for a specific file.
#[cfg(feature = "tree-sitter")]
#[allow(dead_code)]
pub fn clear_kg_edges_for_file(
    conn: &Connection,
    file: &str,
    project_id: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM edges WHERE file = ?1 AND project_id = ?2",
        rusqlite::params![file, project_id],
    )?;
    Ok(())
}

/// Clear call graph edges for a specific file (before re-indexing), scoped to project.
#[allow(dead_code)]
pub fn clear_edges_for_file(conn: &Connection, file: &str, project_id: i64) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM call_graph WHERE file = ?1 AND project_id = ?2",
        rusqlite::params![file, project_id],
    )?;
    Ok(())
}

/// Find all callers of a symbol (who calls this?), scoped to a project.
///
/// Returns symbols that call the given name, grouped by file.
pub fn find_callers(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<CallerResult>> {
    let pattern = format!("%{symbol_name}%");

    let mut stmt = conn.prepare(
        "SELECT DISTINCT cg.caller, cg.file, cg.line
         FROM call_graph cg
         WHERE cg.callee LIKE ?1 AND cg.project_id = ?2
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![pattern, project_id, limit as i64],
        |row| {
            Ok(CallerResult {
                caller: row.get(0)?,
                file: row.get(1)?,
                line: row.get::<_, i64>(2)? as u32,
            })
        },
    )?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Find callers of a symbol across ALL projects (cross-project fallback).
///
/// Used when project-scoped `find_callers` returns empty — the symbol may
/// exist in another indexed project. Returns results with the project root
/// path so the caller can display which project the match came from.
pub fn find_callers_cross_project(
    conn: &Connection,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<CrossProjectCallerResult>> {
    let pattern = format!("%{symbol_name}%");

    let mut stmt = conn.prepare(
        "SELECT DISTINCT cg.caller, cg.file, cg.line, p.root_path
         FROM call_graph cg
         JOIN projects p ON cg.project_id = p.id
         WHERE cg.callee LIKE ?1
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![pattern, limit as i64],
        |row| {
            Ok(CrossProjectCallerResult {
                caller: row.get(0)?,
                file: row.get(1)?,
                line: row.get::<_, i64>(2)? as u32,
                project_root: row.get(3)?,
            })
        },
    )?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Find all callees of a symbol (what does this call?), scoped to a project.
///
/// Returns symbols that are called by the given name.
#[allow(dead_code)]
pub fn find_callees(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<CalleeResult>> {
    let pattern = format!("%{symbol_name}%");

    let mut stmt = conn.prepare(
        "SELECT DISTINCT cg.callee, cg.file, cg.line
         FROM call_graph cg
         WHERE cg.caller LIKE ?1 AND cg.project_id = ?2
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(
        rusqlite::params![pattern, project_id, limit as i64],
        |row| {
            Ok(CalleeResult {
                callee: row.get(0)?,
                file: row.get(1)?,
                line: row.get::<_, i64>(2)? as u32,
            })
        },
    )?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Impact analysis: what breaks if a symbol changes?, scoped to a project.
///
/// Uses reverse traversal: find all callers recursively up to `depth`.
pub fn impact_analysis(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    depth: u32,
) -> anyhow::Result<Vec<ImpactNode>> {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::new();
    let mut current_level = vec![symbol_name.to_string()];
    let mut current_depth = 0u32;

    while current_depth < depth && !current_level.is_empty() {
        let mut next_level = Vec::new();

        for sym in &current_level {
            if !visited.insert(sym.clone()) {
                continue;
            }

            let callers = find_callers(conn, project_id, sym, 100)?;
            for caller in callers {
                let node = ImpactNode {
                    symbol: caller.caller.clone(),
                    file: caller.file.clone(),
                    line: caller.line,
                    depth: current_depth + 1,
                };

                if !visited.contains(&caller.caller) {
                    next_level.push(caller.caller.clone());
                }

                result.push(node);
            }
        }

        current_level = next_level;
        current_depth += 1;
    }

    // Sort by depth then file
    result.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    Ok(result)
}

/// A caller result entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallerResult {
    pub caller: String,
    pub file: String,
    pub line: u32,
}

/// A cross-project caller result (includes which project the caller belongs to).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossProjectCallerResult {
    pub caller: String,
    pub file: String,
    pub line: u32,
    pub project_root: String,
}

/// A callee result entry.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct CalleeResult {
    pub callee: String,
    pub file: String,
    pub line: u32,
}

/// An impact analysis node.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImpactNode {
    pub symbol: String,
    pub file: String,
    pub line: u32,
    pub depth: u32,
}

/// Trace the execution path from a symbol — BFS over call_graph + edges.
///
/// Returns a tree-like structure showing outgoing call chains.
pub fn trace_path(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    depth: u32,
    direction: TraceDirection,
) -> anyhow::Result<Vec<TraceNode>> {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::new();
    let mut current_level = vec![(symbol_name.to_string(), 0u32)];

    visited.insert(symbol_name.to_string());

    while !current_level.is_empty() {
        let mut next_level = Vec::new();

        for (sym, d) in &current_level {
            if *d >= depth {
                continue;
            }

            let neighbors = match direction {
                TraceDirection::Outgoing => find_callees_edges(conn, project_id, sym, 100)?,
                TraceDirection::Incoming => find_callers_edges(conn, project_id, sym, 100)?,
            };

            for neighbor in neighbors {
                let name = match direction {
                    TraceDirection::Outgoing => neighbor.target,
                    TraceDirection::Incoming => neighbor.source,
                };

                let is_new = visited.insert(name.clone());
                let node = TraceNode {
                    symbol: name.clone(),
                    file: neighbor.file,
                    line: neighbor.line,
                    kind: neighbor.kind,
                    depth: d + 1,
                };

                if is_new {
                    next_level.push((name.clone(), d + 1));
                }
                result.push(node);
            }
        }

        current_level = next_level;
    }

    result.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.file.cmp(&b.file)));
    Ok(result)
}

/// Trace direction: follow outgoing calls or incoming callers.
#[derive(Debug, Clone, Copy)]
pub enum TraceDirection {
    Outgoing,
    Incoming,
}

/// A trace node with edge kind info.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceNode {
    pub symbol: String,
    pub file: String,
    pub line: u32,
    pub kind: String,
    pub depth: u32,
}

/// Find outgoing edges from a symbol (uses edges table, falls back to call_graph).
fn find_callees_edges(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<EdgeRow>> {
    let pattern = format!("%{symbol_name}%");

    // Try edges table first (has typed relationships)
    let mut stmt = conn.prepare(
        "SELECT source, kind, target, file, line
         FROM edges
         WHERE source LIKE ?1 AND project_id = ?2
         LIMIT ?3",
    )?;

    let rows: Vec<EdgeRow> = stmt
        .query_map(
            rusqlite::params![pattern, project_id, limit as i64],
            |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    kind: row.get(1)?,
                    target: row.get(2)?,
                    file: row.get(3)?,
                    line: row.get::<_, i64>(4)? as u32,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();

    if !rows.is_empty() {
        return Ok(rows);
    }

    // Fallback to call_graph
    let mut stmt = conn.prepare(
        "SELECT caller, 'CALLS', callee, file, line
         FROM call_graph
         WHERE caller LIKE ?1 AND project_id = ?2
         LIMIT ?3",
    )?;

    let rows: Vec<EdgeRow> = stmt
        .query_map(
            rusqlite::params![pattern, project_id, limit as i64],
            |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    kind: row.get(1)?,
                    target: row.get(2)?,
                    file: row.get(3)?,
                    line: row.get::<_, i64>(4)? as u32,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

/// Find incoming edges to a symbol (uses edges table, falls back to call_graph).
fn find_callers_edges(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<EdgeRow>> {
    let pattern = format!("%{symbol_name}%");

    let mut stmt = conn.prepare(
        "SELECT source, kind, target, file, line
         FROM edges
         WHERE target LIKE ?1 AND project_id = ?2
         LIMIT ?3",
    )?;

    let rows: Vec<EdgeRow> = stmt
        .query_map(
            rusqlite::params![pattern, project_id, limit as i64],
            |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    kind: row.get(1)?,
                    target: row.get(2)?,
                    file: row.get(3)?,
                    line: row.get::<_, i64>(4)? as u32,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();

    if !rows.is_empty() {
        return Ok(rows);
    }

    let mut stmt = conn.prepare(
        "SELECT caller, 'CALLS', callee, file, line
         FROM call_graph
         WHERE callee LIKE ?1 AND project_id = ?2
         LIMIT ?3",
    )?;

    let rows: Vec<EdgeRow> = stmt
        .query_map(
            rusqlite::params![pattern, project_id, limit as i64],
            |row| {
                Ok(EdgeRow {
                    source: row.get(0)?,
                    kind: row.get(1)?,
                    target: row.get(2)?,
                    file: row.get(3)?,
                    line: row.get::<_, i64>(4)? as u32,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();

    Ok(rows)
}

/// Raw edge row from the database.
struct EdgeRow {
    source: String,
    kind: String,
    target: String,
    file: String,
    line: u32,
}

/// Architecture overview: module statistics and edge density.
pub fn arch_overview(conn: &Connection, project_id: i64) -> anyhow::Result<ArchOverview> {
    // Module = directory component of file path (e.g., "src/index" from "src/index/mod.rs")
    let mut stmt = conn.prepare(
        "SELECT
            CASE
                WHEN instr(file, '/') > 0 THEN substr(file, 1, instr(file, '/') - 1)
                ELSE file
            END AS module,
            COUNT(*) AS symbol_count,
            COUNT(DISTINCT kind) AS edge_types
         FROM symbols
         WHERE project_id = ?1
         GROUP BY module
         ORDER BY symbol_count DESC",
    )?;

    let modules: Vec<ModuleInfo> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok(ModuleInfo {
                name: row.get(0)?,
                symbol_count: row.get::<_, i64>(1)? as usize,
                edge_types: row.get::<_, i64>(2)? as usize,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Edge type distribution
    let edge_counts = get_edge_kind_counts(conn, project_id);

    Ok(ArchOverview {
        modules,
        edge_counts,
    })
}

/// Count edges by kind.
fn get_edge_kind_counts(conn: &Connection, project_id: i64) -> Vec<(String, i64)> {
    let mut counts = Vec::new();

    if let Ok(mut s) = conn.prepare(
        "SELECT kind, COUNT(*) FROM edges WHERE project_id = ?1 GROUP BY kind ORDER BY COUNT(*) DESC",
    ) {
        if let Ok(rows) = s.query_map(rusqlite::params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }) {
            counts.extend(rows.flatten());
        }
    }

    if counts.is_empty() {
        if let Ok(mut s) =
            conn.prepare("SELECT 'CALLS', COUNT(*) FROM call_graph WHERE project_id = ?1")
        {
            if let Ok(rows) = s.query_map(rusqlite::params![project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                counts.extend(rows.flatten());
            }
        }
    }

    counts
}

/// Architecture overview result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchOverview {
    pub modules: Vec<ModuleInfo>,
    pub edge_counts: Vec<(String, i64)>,
}

/// Module-level statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModuleInfo {
    pub name: String,
    pub symbol_count: usize,
    pub edge_types: usize,
}
#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        super::super::schema::run_migrations(&conn).unwrap();
        conn
    }

    /// Create a test project and return its project_id.
    fn test_project(conn: &Connection) -> i64 {
        super::super::schema::get_or_create_project(conn, "/tmp/test-project").unwrap()
    }

    #[test]
    fn test_store_and_find_callers() {
        let conn = mem_conn();
        let project_id = test_project(&conn);

        let edges = vec![
            CallEdge {
                caller: "main".to_string(),
                callee: "authenticate".to_string(),
                file: "main.rs".to_string(),
                line: 10,
            },
            CallEdge {
                caller: "handler".to_string(),
                callee: "authenticate".to_string(),
                file: "handler.rs".to_string(),
                line: 25,
            },
        ];
        store_edges(&conn, &edges, project_id).unwrap();

        let callers = find_callers(&conn, project_id, "authenticate", 10).unwrap();
        assert_eq!(callers.len(), 2);
        let names: Vec<&str> = callers.iter().map(|c| c.caller.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"handler"));
    }

    #[test]
    fn test_find_callers_cross_project() {
        let conn = mem_conn();
        let project_a =
            super::super::schema::get_or_create_project(&conn, "/tmp/project-a").unwrap();
        let project_b =
            super::super::schema::get_or_create_project(&conn, "/tmp/project-b").unwrap();

        // Store edges in project A
        store_edges(
            &conn,
            &[CallEdge {
                caller: "handler_a".to_string(),
                callee: "shared_util".to_string(),
                file: "handler.rs".to_string(),
                line: 5,
            }],
            project_a,
        )
        .unwrap();

        // Store edges in project B
        store_edges(
            &conn,
            &[CallEdge {
                caller: "handler_b".to_string(),
                callee: "shared_util".to_string(),
                file: "lib.rs".to_string(),
                line: 10,
            }],
            project_b,
        )
        .unwrap();

        // Scoped to project A: only finds handler_a
        let scoped = find_callers(&conn, project_a, "shared_util", 10).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].caller, "handler_a");

        // Scoped to project B: only finds handler_b
        let scoped_b = find_callers(&conn, project_b, "shared_util", 10).unwrap();
        assert_eq!(scoped_b.len(), 1);
        assert_eq!(scoped_b[0].caller, "handler_b");

        // Cross-project: finds BOTH
        let cross = find_callers_cross_project(&conn, "shared_util", 10).unwrap();
        assert_eq!(cross.len(), 2);
        let roots: Vec<&str> = cross.iter().map(|c| c.project_root.as_str()).collect();
        assert!(roots.contains(&"/tmp/project-a"));
        assert!(roots.contains(&"/tmp/project-b"));
    }

    #[test]
    fn test_find_callees() {
        let conn = mem_conn();
        let project_id = test_project(&conn);

        let edges = vec![
            CallEdge {
                caller: "main".to_string(),
                callee: "init".to_string(),
                file: "main.rs".to_string(),
                line: 5,
            },
            CallEdge {
                caller: "main".to_string(),
                callee: "run".to_string(),
                file: "main.rs".to_string(),
                line: 10,
            },
        ];
        store_edges(&conn, &edges, project_id).unwrap();

        let callees = find_callees(&conn, project_id, "main", 10).unwrap();
        assert_eq!(callees.len(), 2);
    }

    #[test]
    fn test_clear_edges_for_file() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        store_edges(
            &conn,
            &[CallEdge {
                caller: "a".to_string(),
                callee: "b".to_string(),
                file: "test.rs".to_string(),
                line: 1,
            }],
            project_id,
        )
        .unwrap();

        clear_edges_for_file(&conn, "test.rs", project_id).unwrap();
        let callers = find_callers(&conn, project_id, "b", 10).unwrap();
        assert!(callers.is_empty());
    }

    #[test]
    fn test_impact_analysis_depth() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        // a → b → c
        // If c changes, impact should find b (depth 1) and a (depth 2)
        let edges = vec![
            CallEdge {
                caller: "b".to_string(),
                callee: "c".to_string(),
                file: "b.rs".to_string(),
                line: 1,
            },
            CallEdge {
                caller: "a".to_string(),
                callee: "b".to_string(),
                file: "a.rs".to_string(),
                line: 1,
            },
        ];
        store_edges(&conn, &edges, project_id).unwrap();

        let impact = impact_analysis(&conn, project_id, "c", 3).unwrap();
        // Should find b at depth 1, a at depth 2
        assert!(impact.iter().any(|n| n.symbol == "b" && n.depth == 1));
        assert!(impact.iter().any(|n| n.symbol == "a" && n.depth == 2));
    }

    #[test]
    fn test_trace_path_outgoing() {
        let conn = mem_conn();
        let pid = test_project(&conn);

        // a→b→c, a→d
        for (caller, callee) in [("a", "b"), ("b", "c"), ("a", "d")] {
            store_edges(
                &conn,
                &[CallEdge {
                    caller: caller.into(),
                    callee: callee.into(),
                    file: "x.rs".into(),
                    line: 1,
                }],
                pid,
            )
            .unwrap();
        }

        let nodes = trace_path(&conn, pid, "a", 2, TraceDirection::Outgoing).unwrap();
        let syms: Vec<&str> = nodes.iter().map(|n| n.symbol.as_str()).collect();
        assert!(syms.contains(&"b"));
        assert!(syms.contains(&"c"));
        assert!(syms.contains(&"d"));
    }

    #[test]
    fn test_trace_path_incoming() {
        let conn = mem_conn();
        let pid = test_project(&conn);

        for (caller, callee) in [("a", "c"), ("b", "c")] {
            store_edges(
                &conn,
                &[CallEdge {
                    caller: caller.into(),
                    callee: callee.into(),
                    file: "y.rs".into(),
                    line: 1,
                }],
                pid,
            )
            .unwrap();
        }

        let nodes = trace_path(&conn, pid, "c", 1, TraceDirection::Incoming).unwrap();
        let syms: Vec<&str> = nodes.iter().map(|n| n.symbol.as_str()).collect();
        assert!(syms.contains(&"a"));
        assert!(syms.contains(&"b"));
    }

    #[test]
    fn test_arch_overview() {
        let conn = mem_conn();
        let pid = test_project(&conn);

        store_edges(
            &conn,
            &[CallEdge {
                caller: "main".into(),
                callee: "run".into(),
                file: "src/main.rs".into(),
                line: 1,
            }],
            pid,
        )
        .unwrap();

        // Insert a symbol so arch has data
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["main", "function", "src/main.rs", 1i64, pid],
        )
        .unwrap();

        let overview = arch_overview(&conn, pid).unwrap();
        assert_eq!(overview.modules.len(), 1);
        assert_eq!(overview.modules[0].name, "src");
    }
}
