//! Call graph traversal for `cora callers` and `cora impact`.
//!
//! Uses the existing `engine/context/extraction.rs` symbol reference extraction
//! to build call edges at index time, then traverse them for queries.

use rusqlite::Connection;

use super::dead_filters;
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

/// A caller query: the callee name plus an optional type or module it must
/// have been reached through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalleeQuery {
    /// Final name segment (`from_env` for `WebSearchTool::from_env`).
    pub name: String,
    /// Type or module before the name, when the query gave one.
    pub qualifier: Option<String>,
}

impl CalleeQuery {
    /// Parse `name` or `Type::name` (any leading path is reduced to its last
    /// type segment; `Type.name` is accepted too).
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let mut parts = raw.rsplitn(2, "::");
        let name = parts.next().unwrap_or(raw);
        let qualifier = parts
            .next()
            .and_then(|q| q.rsplit("::").next())
            .map(|q| q.split('<').next().unwrap_or(q).trim())
            .filter(|q| !q.is_empty());
        Self {
            name: name.to_string(),
            qualifier: qualifier.map(str::to_string),
        }
    }
}

/// Find all callers of a symbol (who calls this?), scoped to a project.
///
/// `symbol_name` is a bare name (every call to that name, as before), a
/// `Type::name` (only calls through `Type`, plus ambiguous calls whose
/// receiver type is unknown) or a bare type name (every call through it, and
/// calls to a tuple-struct constructor of that name).
pub fn find_callers(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<CallerResult>> {
    find_callers_opts(conn, project_id, symbol_name, limit, false)
}

/// `find_callers` with `strict`: leave out ambiguous edges (no known
/// qualifier) for a type-qualified query.
pub fn find_callers_opts(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
    strict: bool,
) -> anyhow::Result<Vec<CallerResult>> {
    let q = CalleeQuery::parse(symbol_name);
    let map = |row: &rusqlite::Row| -> rusqlite::Result<CallerResult> {
        Ok(CallerResult {
            caller: row.get(0)?,
            file: row.get(1)?,
            line: row.get::<_, i64>(2)? as u32,
            qualifier: row.get(3)?,
            caller_type: row.get(4)?,
        })
    };
    let rows: Vec<CallerResult> = match &q.qualifier {
        Some(qual) => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT cg.caller, cg.file, cg.line, cg.qualifier, cg.caller_type
                 FROM call_graph cg
                 WHERE cg.callee = ?1 AND cg.project_id = ?2
                   AND (cg.qualifier = ?3 OR (cg.qualifier IS NULL AND ?4 = 0))
                 LIMIT ?5",
            )?;
            stmt.query_map(
                rusqlite::params![q.name, project_id, qual, strict as i64, limit as i64],
                map,
            )?
            .filter_map(|r| r.ok())
            .collect()
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT cg.caller, cg.file, cg.line, cg.qualifier, cg.caller_type
                 FROM call_graph cg
                 WHERE (cg.callee = ?1 OR cg.qualifier = ?1) AND cg.project_id = ?2
                 LIMIT ?3",
            )?;
            stmt.query_map(rusqlite::params![q.name, project_id, limit as i64], map)?
                .filter_map(|r| r.ok())
                .collect()
        }
    };
    Ok(rows)
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
    let pattern = symbol_name;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT cg.caller, cg.file, cg.line, p.root_path
         FROM call_graph cg
         JOIN projects p ON cg.project_id = p.id
         WHERE cg.callee = ?1
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
        Ok(CrossProjectCallerResult {
            caller: row.get(0)?,
            file: row.get(1)?,
            line: row.get::<_, i64>(2)? as u32,
            project_root: row.get(3)?,
        })
    })?;

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
    let pattern = symbol_name;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT cg.callee, cg.file, cg.line
         FROM call_graph cg
         WHERE cg.caller = ?1 AND cg.project_id = ?2
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
    // A node is a method name plus, when known, the impl type it belongs to,
    // so two `from_env` of different types are followed separately (#714).
    let seed = CalleeQuery::parse(symbol_name);
    let key = |name: &str, ty: Option<&str>| match ty {
        Some(t) => format!("{t}::{name}"),
        None => name.to_string(),
    };
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::new();
    let mut current_level: Vec<(String, Option<String>)> = vec![(seed.name, seed.qualifier)];
    let mut current_depth = 0u32;

    while current_depth < depth && !current_level.is_empty() {
        let mut next_level = Vec::new();

        for (sym, ty) in &current_level {
            if !visited.insert(key(sym, ty.as_deref())) {
                continue;
            }

            let callers = find_callers(conn, project_id, &key(sym, ty.as_deref()), 100)?;
            for caller in callers {
                let node = ImpactNode {
                    symbol: caller.caller.clone(),
                    file: caller.file.clone(),
                    line: caller.line,
                    depth: current_depth + 1,
                };

                if !visited.contains(&key(&caller.caller, caller.caller_type.as_deref())) {
                    next_level.push((caller.caller.clone(), caller.caller_type.clone()));
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
    /// Type or module the call went through; `None` means the receiver type
    /// is unknown, so the edge is ambiguous for a type-qualified query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    /// Impl type of the calling method, when it is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_type: Option<String>,
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

/// What the call graph knows about one call site: the type the call went
/// through and the impl type of the calling method. `(None, None)` when the
/// row is missing, which makes the edge ambiguous.
fn call_site_types(
    conn: &Connection,
    project_id: i64,
    edge: &EdgeRow,
) -> (Option<String>, Option<String>) {
    conn.query_row(
        "SELECT qualifier, caller_type FROM call_graph
         WHERE caller = ?1 AND callee = ?2 AND file = ?3 AND line = ?4 AND project_id = ?5
         LIMIT 1",
        rusqlite::params![
            edge.source,
            edge.target,
            edge.file,
            edge.line as i64,
            project_id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap_or((None, None))
}

#[allow(dead_code)]
/// Trace the execution path from a symbol — BFS over call_graph + edges.
///
/// `symbol_name` may be `Type::name`; each hop then carries the type of the
/// node it reached (the qualifier of a call, the impl type of a calling
/// method), so same-named methods of other types are not merged (#743).
pub fn trace_path(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    depth: u32,
    direction: TraceDirection,
) -> anyhow::Result<Vec<TraceNode>> {
    trace_path_opts(conn, project_id, symbol_name, depth, direction, false)
}

/// `trace_path` with `strict`: leave out ambiguous call edges (receiver or
/// caller type unknown) once a node has a known type.
pub fn trace_path_opts(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    depth: u32,
    direction: TraceDirection,
    strict: bool,
) -> anyhow::Result<Vec<TraceNode>> {
    let key = |name: &str, ty: Option<&str>| match ty {
        Some(t) => format!("{t}::{name}"),
        None => name.to_string(),
    };
    let seed = CalleeQuery::parse(symbol_name);
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::new();
    let mut current_level: Vec<(String, Option<String>, u32)> =
        vec![(seed.name.clone(), seed.qualifier.clone(), 0u32)];

    visited.insert(key(&seed.name, seed.qualifier.as_deref()));

    while !current_level.is_empty() {
        let mut next_level = Vec::new();

        for (sym, ty, d) in &current_level {
            if *d >= depth {
                continue;
            }

            // A typed node needs a wider scan: same-named methods of other
            // types fill the cap before the right ones are reached.
            let limit = if ty.is_some() { 2000 } else { 100 };
            let neighbors = match direction {
                TraceDirection::Outgoing => find_callees_edges(conn, project_id, sym, limit)?,
                TraceDirection::Incoming => find_callers_edges(conn, project_id, sym, limit)?,
            };

            for neighbor in neighbors {
                // The type the neighbor is reached as; `None` is unknown.
                let mut next_ty: Option<String> = None;
                if neighbor.kind == "CALLS" {
                    let (qualifier, caller_type) = call_site_types(conn, project_id, &neighbor);
                    let (matches_ty, reached_as) = match direction {
                        // Calls to `sym` must have gone through `ty`; the
                        // caller is the node reached, as its impl type.
                        TraceDirection::Incoming => (qualifier.clone(), caller_type.clone()),
                        // Calls made by `sym` must come from a method of `ty`;
                        // the callee is reached through the qualifier.
                        TraceDirection::Outgoing => (caller_type.clone(), qualifier.clone()),
                    };
                    if let Some(want) = ty.as_deref() {
                        match matches_ty.as_deref() {
                            Some(have) if have != want => continue,
                            None if strict => continue,
                            _ => {}
                        }
                    }
                    next_ty = reached_as;
                }

                let name = match direction {
                    TraceDirection::Outgoing => neighbor.target,
                    TraceDirection::Incoming => neighbor.source,
                };

                let is_new = visited.insert(key(&name, next_ty.as_deref()));
                let node = TraceNode {
                    symbol: name.clone(),
                    file: neighbor.file,
                    line: neighbor.line,
                    kind: neighbor.kind,
                    depth: d + 1,
                    qualifier: next_ty.clone(),
                };

                if is_new {
                    next_level.push((name.clone(), next_ty, d + 1));
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
    /// Type the node was reached as, when the call graph knows it (#743).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
}

/// Find outgoing edges from a symbol (uses edges table, falls back to call_graph).
fn find_callees_edges(
    conn: &Connection,
    project_id: i64,
    symbol_name: &str,
    limit: usize,
) -> anyhow::Result<Vec<EdgeRow>> {
    let pattern = symbol_name;

    // Try edges table first (has typed relationships)
    let mut stmt = conn.prepare(
        "SELECT source, kind, target, file, line
         FROM edges
         WHERE source = ?1 AND project_id = ?2
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
         WHERE caller = ?1 AND project_id = ?2
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
    let pattern = symbol_name;

    let mut stmt = conn.prepare(
        "SELECT source, kind, target, file, line
         FROM edges
         WHERE target = ?1 AND project_id = ?2
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
         WHERE callee = ?1 AND project_id = ?2
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

/// Count edges by kind.
pub(super) fn get_edge_kind_counts(conn: &Connection, project_id: i64) -> Vec<(String, i64)> {
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

/// A dead-code detection result entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeadCodeResult {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
    pub reason: String,
}

/// Options controlling dead-code detection.
#[derive(Debug, Clone, Default)]
pub struct DeadCodeOptions {
    /// Include test functions (starting with `test_` or `_test`) in dead code detection.
    pub include_tests: bool,
    /// If set, only report symbols whose span is at least this many lines long.
    pub min_lines: Option<u32>,
    /// Additional entry-point method name patterns to exclude from dead-code.
    /// Each entry is a SQL LIKE pattern (e.g. `%Handler`, `%Listener`, `%Route`).
    /// These are checked against symbol names in addition to WELL_KNOWN_NAMES.
    pub entry_point_patterns: Vec<String>,
    /// Include public API surface (`pub`/`export` items) in dead code results.
    /// By default these are skipped: their purpose is external consumption,
    /// so absence of internal callers does not make them dead (#520).
    pub include_pub_api: bool,
    /// Project root, used to read Rust sources for the `#[test]`, `cfg(test)`
    /// and trait-impl rules. Without it only the path/name rules apply.
    pub project_root: Option<std::path::PathBuf>,
}

/// Well-known symbol names that should not be flagged as dead code even when
/// they have no recorded callers (they are typically called via trait
/// dispatch, language builtins, or external entry points).
const WELL_KNOWN_NAMES: &[&str] = &[
    "main",
    "new",
    "drop",
    "default",
    "clone",
    "eq",
    "hash",
    "fmt",
    "debug",
    "display",
    "from_str",
    "into",
    "from",
    "try_from",
    "try_into",
    "deref",
    "deref_mut",
    "index",
    "index_mut",
    "add",
    "sub",
    "mul",
    "div",
    "rem",
    "neg",
    "not",
    "bitand",
    "bitor",
    "bitxor",
    "shl",
    "shr",
    "partial_eq",
    "partial_ord",
    "eq_ignore_ascii_case",
    "is_ascii",
    "to_lowercase",
    "to_uppercase",
    "trim",
    "trim_start",
    "trim_end",
    "as_str",
    "as_bytes",
    "as_mut",
    "as_ref",
    "into_iter",
    "iter",
    "next",
    "map",
    "filter",
    "fold",
    "collect",
    "len",
    "is_empty",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
    "get",
    "get_mut",
    "with_capacity",
    "to_vec",
    "to_string",
    "parse",
    "unwrap",
    "expect",
    "ok",
    "err",
    "is_ok",
    "is_err",
    "is_some",
    "is_none",
    "take",
    "replace",
    "lock",
    "read",
    "write",
    "flush",
    "close",
    "connect",
    "accept",
    "bind",
    "listen",
    "send",
    "recv",
    "shutdown",
    "spawn",
    "join",
    "sleep",
    "yield_now",
    "block_on",
    "from_secs",
    "from_millis",
    "elapsed",
    "instant",
    "system_time",
    "now",
    "duration_since",
];

/// Framework lifecycle / entry-point method name prefixes.
///
/// Methods starting with these prefixes are typically called by frameworks
/// (Axum route handlers, SvelteKit page loads, Phaser scene lifecycle, etc.)
/// and may not have direct callers in the codebase.
const FRAMEWORK_ENTRY_PREFIXES: &[&str] = &[
    // Axum / Actix / Tower handler patterns
    "handle_",
    // SvelteKit page/layout exports
    "load",
    // Phaser scene lifecycle
    "create",
    "preload",
    "update",
    // React / Svelte lifecycle
    "render",
    "component_did_mount",
    "on_mount",
    "on_destroy",
    // General async patterns
    "handle",
    "process",
    "execute",
    "serve",
    "start",
    "stop",
    "run",
    // GraphQL resolvers
    "resolve_",
];

/// Patterns that indicate a method is a framework callback (suffix-based).
const FRAMEWORK_ENTRY_SUFFIXES: &[&str] = &[
    // Axum handler trait
    "Handler",
    // Event listeners
    "Listener",
    "Callback",
    // Middleware
    "Middleware",
    // Route
    "Route",
    // Hook
    "Hook",
    // Plugin
    "Plugin",
    // Provider
    "Provider",
];

/// Find dead code: symbols that have no recorded callers and are not
/// well-known names or (by default) test functions.
///
/// `find_dead_code` looks for `function`, `method`, and `procedure` symbols
/// that never appear as a `callee` in the `call_graph` table for the given
/// project. Results are ordered by file then line.
pub fn find_dead_code(
    conn: &Connection,
    project_id: i64,
    opts: &DeadCodeOptions,
) -> anyhow::Result<Vec<DeadCodeResult>> {
    // Build the `NOT IN (...)` placeholder list for well-known names.
    let placeholders: Vec<&str> = WELL_KNOWN_NAMES.iter().map(|_| "?").collect();
    let not_in_clause = placeholders.join(", ");

    // Build the base SQL. We use dynamic bind parameters: project_id (twice),
    // then the well-known names, then optionally test-name LIKE patterns.
    let mut sql = String::from(
        "SELECT s.name, s.kind, s.file, s.line, s.signature
         FROM symbols s
         WHERE s.project_id = ?1
           AND s.kind IN ('function', 'method', 'procedure')
           AND s.name NOT IN (",
    );
    sql.push_str(&not_in_clause);
    sql.push_str(
        ")
           AND NOT EXISTS (
               SELECT 1 FROM call_graph cg
               WHERE cg.callee = s.name AND cg.project_id = ?1
           )",
    );

    // Exclude test functions unless requested.
    if !opts.include_tests {
        sql.push_str(" AND s.name NOT LIKE 'test_%' AND s.name NOT LIKE '%_test'");
    }

    // Exclude symbols with `// cora: keep` suppression marker in their signature.
    sql.push_str(" AND (s.signature IS NULL OR s.signature NOT LIKE '%cora: keep%')");

    // Exclude framework entry-point methods by name prefix.
    for prefix in FRAMEWORK_ENTRY_PREFIXES {
        sql.push_str(&format!(" AND LOWER(s.name) NOT LIKE '{prefix}%'"));
    }

    // Exclude framework entry-point methods by name suffix.
    for suffix in FRAMEWORK_ENTRY_SUFFIXES {
        sql.push_str(&format!(" AND s.name NOT LIKE '%{suffix}'"));
    }

    // Exclude user-configured entry-point patterns via post-filter below.
    let _ = &opts.entry_point_patterns; // patterns applied in post-filter

    // Optional min_lines filter — applied post-query below when span metadata
    // becomes available. Kept for API compatibility.
    if opts.min_lines.is_some() {
        // Placeholder: span filtering not yet supported (no start/end_line cols)
    }

    sql.push_str(" ORDER BY s.file, s.line");

    let mut stmt = conn.prepare(&sql)?;

    // Bind parameters in order.
    // Bind parameters: ?1 = project_id, followed by well-known names.
    let project_id_ref = &project_id;

    // rusqlite's `query_map` with `params!` macro is awkward for a dynamic
    // number of args, so build a `Vec<&dyn rusqlite::ToSql>`.
    let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(1 + WELL_KNOWN_NAMES.len());
    params_vec.push(project_id_ref);
    for name in WELL_KNOWN_NAMES {
        params_vec.push(name);
    }

    let rows = stmt.query_map(params_vec.as_slice(), |row| {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let file: String = row.get(2)?;
        let line: u32 = row.get::<_, i64>(3)? as u32;
        let signature: Option<String> = row.get(4)?;
        Ok(DeadCodeResult {
            name,
            kind,
            file,
            line,
            signature,
            reason: "no callers found".to_string(),
        })
    })?;

    // Default exclusions that need more than SQL (#716).
    let known_files = if opts.include_pub_api {
        Default::default()
    } else {
        dead_filters::load_known_files(conn, project_id)?
    };
    let mut sources = opts
        .project_root
        .as_deref()
        .map(dead_filters::SourceCache::new);
    let mut rows: Vec<DeadCodeResult> = rows.filter_map(|r| r.ok()).collect();
    rows.retain(|r| {
        let sig = r.signature.as_deref().unwrap_or("");
        let is_rust = r.file.ends_with(".rs");
        if !opts.include_tests && dead_filters::is_test_path(&r.file) {
            return false;
        }
        // Public API: a Rust `pub` item is external surface only when its
        // crate has a lib target; in a bin-only crate nothing is consumed
        // from outside. `export` (TS/JS) is always public.
        if !opts.include_pub_api && dead_filters::is_public_signature(sig) {
            let exposed = if is_rust && !sig.trim_start().starts_with("export ") {
                dead_filters::lib_root_for(&r.file).is_some_and(|l| known_files.contains(&l))
            } else {
                true
            };
            if exposed {
                return false;
            }
        }
        if is_rust
            && let Some(ctx) = sources
                .as_mut()
                .and_then(|s| s.rust_context(&r.file, r.line))
        {
            // Trait methods are reached through dispatch: always skipped.
            if ctx.is_trait_method || (ctx.is_test && !opts.include_tests) {
                return false;
            }
        }
        true
    });

    // Post-filter by user-configured entry_point_patterns (safe LIKE matching).
    let results: Vec<DeadCodeResult> = rows
        .into_iter()
        .filter(|r| {
            // Skip if any user pattern matches
            let name_lower = r.name.to_lowercase();
            !opts.entry_point_patterns.iter().any(|p| {
                // Simple glob-like matching: prefix*, *suffix, or exact
                if p.starts_with('*') && p.ends_with('*') && p.len() > 2 {
                    name_lower.contains(&p[1..p.len() - 1].to_lowercase())
                } else if let Some(suffix) = p.strip_prefix('*') {
                    name_lower.ends_with(&suffix.to_lowercase())
                } else if let Some(prefix) = p.strip_suffix('*') {
                    name_lower.starts_with(&prefix.to_lowercase())
                } else {
                    name_lower == p.to_lowercase()
                }
            })
        })
        .collect();

    Ok(results)
}

/// Result of unused import analysis for a single file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnusedImportResult {
    /// The imported symbol or module name.
    pub target: String,
    /// The source (importing) symbol — usually the file module or import statement.
    pub source: String,
    /// File containing the unused import.
    pub file: String,
    /// Line number of the import statement.
    pub line: u32,
}

/// Find unused imports in a specific file using the edges table.
///
/// For each IMPORTS edge in the file, checks if the imported symbol is
/// referenced anywhere else (as a callee in call_graph, or as source/target
/// in non-IMPORTS edges). If not referenced → unused import.
pub fn find_unused_imports(
    conn: &Connection,
    file: &str,
    project_id: i64,
) -> anyhow::Result<Vec<UnusedImportResult>> {
    // 1. Get all IMPORTS edges for this file
    let mut imports_stmt = conn.prepare(
        "SELECT source, target, line FROM edges
         WHERE file = ?1 AND kind = 'IMPORTS' AND project_id = ?2",
    )?;

    let imports: Vec<(String, String, u32)> = imports_stmt
        .query_map(rusqlite::params![file, project_id], |row| {
            let source: String = row.get(0)?;
            let target: String = row.get(1)?;
            let line: u32 = row.get::<_, i64>(2)? as u32;
            Ok((source, target, line))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if imports.is_empty() {
        return Ok(Vec::new());
    }

    let mut unused = Vec::new();

    for (source, target, line) in &imports {
        // Check if the imported target is used in this file:
        // - As callee in call_graph
        // - As source or target in non-IMPORTS edges within this file
        let is_used = conn.query_row(
            "SELECT COUNT(*) FROM (
                SELECT 1 FROM call_graph cg
                WHERE cg.callee = ?1 AND cg.file = ?2 AND cg.project_id = ?3
                UNION
                SELECT 1 FROM edges e
                WHERE e.target = ?1 AND e.file = ?2 AND e.kind != 'IMPORTS' AND e.project_id = ?3
            )",
            rusqlite::params![target, file, project_id],
            |row| row.get::<_, i64>(0),
        )? > 0;

        if !is_used {
            // Additional check: is the target referenced as a symbol in any
            // edge from this file (could be a type used in signatures, etc.)
            let is_referenced = conn.query_row(
                "SELECT COUNT(*) FROM edges e
                 WHERE (e.source = ?1 OR e.target = ?1)
                   AND e.file = ?2
                   AND e.project_id = ?3
                   AND e.kind != 'IMPORTS'",
                rusqlite::params![target, file, project_id],
                |row| row.get::<_, i64>(0),
            )?;

            if is_referenced == 0 {
                unused.push(UnusedImportResult {
                    target: target.clone(),
                    source: source.clone(),
                    file: file.to_string(),
                    line: *line,
                });
            }
        }
    }

    Ok(unused)
}

/// Find dead code (symbols with no callers) within a specific file.
///
/// Unlike the standalone `find_dead_code` which scans the whole project,
/// this function is scoped to changed files for use in review context.
pub fn find_dead_code_in_file(
    conn: &Connection,
    file: &str,
    project_id: i64,
    include_tests: bool,
) -> anyhow::Result<Vec<DeadCodeResult>> {
    let mut sql = String::from(
        "SELECT s.name, s.kind, s.file, s.line, s.signature
         FROM symbols s
         WHERE s.file = ?1
           AND s.project_id = ?2
           AND s.kind IN ('function', 'method', 'procedure')
           AND s.name NOT IN (",
    );

    let placeholders: Vec<&str> = WELL_KNOWN_NAMES.iter().map(|_| "?").collect();
    sql.push_str(&placeholders.join(", "));
    sql.push_str(
        ")
           AND NOT EXISTS (
               SELECT 1 FROM call_graph cg
               WHERE cg.callee = s.name AND cg.project_id = ?2
           )",
    );

    if !include_tests {
        sql.push_str(" AND s.name NOT LIKE 'test_%' AND s.name NOT LIKE '%_test'");
    }

    sql.push_str(" ORDER BY s.line");

    let mut stmt = conn.prepare(&sql)?;

    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
        Vec::with_capacity(2 + WELL_KNOWN_NAMES.len());
    params_vec.push(Box::new(file.to_string()));
    params_vec.push(Box::new(project_id));
    for name in WELL_KNOWN_NAMES {
        params_vec.push(Box::new(*name));
    }

    let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();

    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let file: String = row.get(2)?;
        let line: u32 = row.get::<_, i64>(3)? as u32;
        let signature: Option<String> = row.get(4)?;
        Ok(DeadCodeResult {
            name,
            kind,
            file,
            line,
            signature,
            reason: "no callers found".to_string(),
        })
    })?;

    Ok(rows.filter_map(|r| r.ok()).collect())
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
    fn test_find_dead_code() {
        let conn = mem_conn();
        let pid = test_project(&conn);

        // Insert symbols:
        //   - "used_fn" (function) → will have a caller, NOT dead
        //   - "orphan_fn" (function) → no caller, NOT well-known → DEAD
        //   - "unused_method" (method) → no caller → DEAD
        //   - "test_unused" (function) → no caller but starts with test_ →
        //     excluded by default, included when include_tests=true
        //   - "new" (function) → no caller but well-known → NOT dead
        let insert_sym = |name: &str, kind: &str, file: &str, line: i64| {
            conn.execute(
                "INSERT INTO symbols (name, kind, file, line, signature, language, project_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![name, kind, file, line, "", "rust", pid],
            )
            .unwrap();
        };

        insert_sym("used_fn", "function", "src/lib.rs", 10);
        insert_sym("orphan_fn", "function", "src/lib.rs", 20);
        insert_sym("unused_method", "method", "src/impl.rs", 5);
        insert_sym("test_unused", "function", "src/tests.rs", 1);
        insert_sym("new", "function", "src/lib.rs", 30);

        // Add a call edge: someone calls used_fn
        store_edges(
            &conn,
            &[CallEdge {
                caller: "caller_fn".into(),
                callee: "used_fn".into(),
                file: "src/lib.rs".into(),
                line: 50,
            }],
            pid,
        )
        .unwrap();

        // Default opts: test functions excluded
        let dead = find_dead_code(&conn, pid, &DeadCodeOptions::default()).unwrap();
        let names: Vec<&str> = dead.iter().map(|d| d.name.as_str()).collect();

        // orphan_fn and unused_method are dead; used_fn has a caller;
        // new is well-known; test_unused is excluded as a test function.
        assert!(names.contains(&"orphan_fn"));
        assert!(names.contains(&"unused_method"));
        assert!(!names.contains(&"used_fn"));
        assert!(!names.contains(&"new"));
        assert!(!names.contains(&"test_unused"));

        // With include_tests = true, test_unused should appear
        let dead_with_tests = find_dead_code(
            &conn,
            pid,
            &DeadCodeOptions {
                include_tests: true,
                ..Default::default()
            },
        )
        .unwrap();
        let names_t: Vec<&str> = dead_with_tests.iter().map(|d| d.name.as_str()).collect();
        assert!(names_t.contains(&"test_unused"));
        assert!(names_t.contains(&"orphan_fn"));

        // Verify result fields
        let orphan = dead.iter().find(|d| d.name == "orphan_fn").unwrap();
        assert_eq!(orphan.kind, "function");
        assert_eq!(orphan.file, "src/lib.rs");
        assert_eq!(orphan.line, 20);
        assert_eq!(orphan.reason, "no callers found");
    }

    /// Regression (#519): a symbol defined in one crate/file and called via
    /// method syntax (`obj.remember_with_contradiction()`) from another
    /// crate's file must NOT be flagged dead. Mirrors the real uteke
    /// workspace case: definition in uteke-core, caller in uteke-cli.
    #[test]
    fn test_dead_code_resolves_cross_file_method_calls() {
        use super::super::index_file;

        let conn = mem_conn();
        let pid = test_project(&conn);

        // "uteke-core": the definition.
        index_file(
            &conn,
            pid,
            "crates/core/src/consolidate.rs",
            r#"
pub fn remember_with_contradiction(content: &str) -> usize { content.len() }
"#,
            "rs",
        )
        .unwrap();

        // "uteke-cli": a cross-crate caller using method syntax.
        index_file(
            &conn,
            pid,
            "crates/cli/src/commands/maintenance.rs",
            r#"
pub fn maintenance() -> usize {
    let store = Store;
    store.remember_with_contradiction("note")
}
"#,
            "rs",
        )
        .unwrap();

        let dead = find_dead_code(&conn, pid, &DeadCodeOptions::default()).unwrap();
        assert!(
            !dead.iter().any(|d| d.name == "remember_with_contradiction"),
            "cross-crate method call must prevent false-positive dead code, got: {:?}",
            dead.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
    }

    fn call(caller: &str, callee: &str, file: &str) -> CallEdge {
        CallEdge {
            caller: caller.to_string(),
            callee: callee.to_string(),
            file: file.to_string(),
            line: 1,
        }
    }

    /// Regression: callers were matched with `LIKE '%name%'`, so removing
    /// `run` flagged callers of `rerun`, and `_`/`%` acted as wildcards.
    fn typed_call(conn: &Connection, pid: i64, caller: &str, callee: &str, q: Option<&str>) {
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id, qualifier) \
             VALUES (?1, ?2, 'a.rs', 1, ?3, ?4)",
            rusqlite::params![caller, callee, pid, q],
        )
        .unwrap();
    }

    /// #714: `callers Type::method` lists only calls through that type, plus
    /// ambiguous ones unless strict; a bare name still lists every call.
    #[test]
    fn test_find_callers_type_qualified() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_call(&conn, pid, "roster_a", "from_env", Some("WebSearchTool"));
        typed_call(&conn, pid, "roster_b", "from_env", Some("WebFetchTool"));
        typed_call(&conn, pid, "untyped", "from_env", None);

        let names = |v: Vec<CallerResult>| {
            let mut n: Vec<String> = v.into_iter().map(|c| c.caller).collect();
            n.sort();
            n
        };
        assert_eq!(
            names(find_callers(&conn, pid, "from_env", 50).unwrap()).len(),
            3
        );
        assert_eq!(
            names(find_callers(&conn, pid, "WebSearchTool::from_env", 50).unwrap()),
            ["roster_a", "untyped"]
        );
        assert_eq!(
            names(find_callers_opts(&conn, pid, "WebSearchTool::from_env", 50, true).unwrap()),
            ["roster_a"]
        );
    }

    /// #714: a bare type name finds the calls made through the type.
    #[test]
    fn test_find_callers_of_a_type_name() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_edge(
            &conn,
            pid,
            "roster",
            None,
            "from_env",
            Some("WebFetchTool"),
            1,
        );
        typed_edge(
            &conn,
            pid,
            "roster",
            None,
            "default",
            Some("WebFetchTool"),
            2,
        );
        typed_edge(&conn, pid, "elsewhere", None, "default", Some("Other"), 3);

        let callers = find_callers(&conn, pid, "WebFetchTool", 50).unwrap();
        assert_eq!(callers.len(), 2);
        assert!(callers.iter().all(|c| c.caller == "roster"));
    }

    /// #714: impact follows the typed node, so another type's same-named method
    /// does not widen the result.
    #[test]
    fn test_impact_follows_typed_nodes() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_call(&conn, pid, "a_user", "from_env", Some("A"));
        typed_call(&conn, pid, "b_user", "from_env", Some("B"));
        typed_call(&conn, pid, "top_a", "a_user", None);
        typed_call(&conn, pid, "top_b", "b_user", None);

        let nodes = impact_analysis(&conn, pid, "A::from_env", 3).unwrap();
        let syms: Vec<&str> = nodes.iter().map(|n| n.symbol.as_str()).collect();
        assert_eq!(syms, ["a_user", "top_a"]);
    }

    fn typed_edge(
        conn: &Connection,
        pid: i64,
        caller: &str,
        caller_type: Option<&str>,
        callee: &str,
        qualifier: Option<&str>,
        line: i64,
    ) {
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id, qualifier, caller_type) \
             VALUES (?1, ?2, 'a.rs', ?3, ?4, ?5, ?6)",
            rusqlite::params![caller, callee, line, pid, qualifier, caller_type],
        )
        .unwrap();
    }

    /// #743: tracing callers of `A::from_env` follows only calls through `A`,
    /// and each hop continues from the caller's own type.
    #[test]
    fn test_trace_incoming_follows_typed_nodes() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_edge(&conn, pid, "run", Some("RosterA"), "from_env", Some("A"), 1);
        typed_edge(&conn, pid, "run", Some("RosterB"), "from_env", Some("B"), 2);
        typed_edge(&conn, pid, "top_a", None, "run", Some("RosterA"), 3);
        typed_edge(&conn, pid, "top_b", None, "run", Some("RosterB"), 4);

        let nodes = trace_path(&conn, pid, "A::from_env", 3, TraceDirection::Incoming).unwrap();
        let lines: Vec<u32> = nodes.iter().map(|n| n.line).collect();
        assert_eq!(lines, [1, 3], "only the A chain: {nodes:?}");

        // The bare name still merges every type, as before.
        let all = trace_path(&conn, pid, "from_env", 1, TraceDirection::Incoming).unwrap();
        assert_eq!(all.len(), 2);
    }

    /// #743: tracing callees of `RosterA::run` ignores another type's `run`.
    #[test]
    fn test_trace_outgoing_follows_typed_nodes() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_edge(&conn, pid, "run", Some("RosterA"), "build", Some("A"), 1);
        typed_edge(&conn, pid, "run", Some("RosterB"), "build", Some("B"), 2);
        typed_edge(&conn, pid, "build", Some("A"), "leaf_a", None, 3);
        typed_edge(&conn, pid, "build", Some("B"), "leaf_b", None, 4);

        let nodes = trace_path(&conn, pid, "RosterA::run", 3, TraceDirection::Outgoing).unwrap();
        let lines: Vec<u32> = nodes.iter().map(|n| n.line).collect();
        assert_eq!(lines, [1, 3], "only RosterA's chain: {nodes:?}");
    }

    /// #743: `--strict` drops calls whose type is unknown for a typed node.
    #[test]
    fn test_trace_strict_drops_ambiguous_edges() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        typed_edge(&conn, pid, "known", None, "from_env", Some("A"), 1);
        typed_edge(&conn, pid, "untyped", None, "from_env", None, 2);

        let loose = trace_path_opts(
            &conn,
            pid,
            "A::from_env",
            1,
            TraceDirection::Incoming,
            false,
        )
        .unwrap();
        let strict =
            trace_path_opts(&conn, pid, "A::from_env", 1, TraceDirection::Incoming, true).unwrap();
        assert_eq!(loose.len(), 2);
        assert_eq!(strict.len(), 1);
        assert_eq!(strict[0].symbol, "known");
    }

    #[test]
    fn test_callee_query_parse() {
        let q = CalleeQuery::parse("crate::web::WebTool<T>::run");
        assert_eq!(
            (q.name.as_str(), q.qualifier.as_deref()),
            ("run", Some("WebTool"))
        );
        let q = CalleeQuery::parse("run");
        assert_eq!((q.name.as_str(), q.qualifier), ("run", None));
    }

    #[test]
    fn test_find_callers_exact_match_only() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        store_edges(
            &conn,
            &[
                call("a", "run", "a.rs"),
                call("b", "rerun", "b.rs"),
                call("c", "get_users", "c.rs"),
                call("d", "getXusers", "d.rs"),
            ],
            pid,
        )
        .unwrap();

        let callers = find_callers(&conn, pid, "run", 100).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller, "a");

        // `_` must not behave as a single-char wildcard.
        let callers = find_callers(&conn, pid, "get_users", 100).unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller, "c");

        // `%` must not match everything.
        assert!(find_callers(&conn, pid, "%", 100).unwrap().is_empty());
        assert!(
            find_callers_cross_project(&conn, "run", 100)
                .unwrap()
                .iter()
                .all(|r| r.caller == "a")
        );
    }

    /// Exact matching must not break transitive traversal.
    #[test]
    fn test_impact_analysis_and_trace_exact_recursion() {
        let conn = mem_conn();
        let pid = test_project(&conn);
        store_edges(
            &conn,
            &[
                call("mid", "run", "m.rs"),
                call("top", "mid", "t.rs"),
                call("other", "rerun", "o.rs"),
            ],
            pid,
        )
        .unwrap();

        let impact = impact_analysis(&conn, pid, "run", 3).unwrap();
        let names: Vec<&str> = impact.iter().map(|n| n.symbol.as_str()).collect();
        assert_eq!(names, vec!["mid", "top"]);

        let trace = trace_path(&conn, pid, "run", 3, TraceDirection::Incoming).unwrap();
        let names: Vec<&str> = trace.iter().map(|n| n.symbol.as_str()).collect();
        assert_eq!(names, vec!["mid", "top"]);
    }

    /// Regression (#520): public API surface is skipped by default — missing
    /// internal callers does not make a `pub` item dead — and can be opted
    /// back in with `include_pub_api`. Private helpers still get flagged.
    #[test]
    fn test_dead_code_skips_pub_api_by_default() {
        use super::super::index_file;

        let conn = mem_conn();
        let pid = test_project(&conn);

        index_file(
            &conn,
            pid,
            "src/lib.rs",
            r#"
pub async fn chunk_markdown_embed_aware(text: &str) -> usize { text.len() }

fn internal_only_helper() -> u8 { 7 }
"#,
            "rs",
        )
        .unwrap();

        // Default: pub items are treated as API surface and skipped.
        let dead = find_dead_code(&conn, pid, &DeadCodeOptions::default()).unwrap();
        let names: Vec<&str> = dead.iter().map(|d| d.name.as_str()).collect();
        assert!(
            !names.contains(&"chunk_markdown_embed_aware"),
            "pub fn must be skipped by default"
        );
        assert!(
            names.contains(&"internal_only_helper"),
            "private helper without callers is dead"
        );

        // Opt-in: pub items are reported again.
        let all = find_dead_code(
            &conn,
            pid,
            &DeadCodeOptions {
                include_pub_api: true,
                ..Default::default()
            },
        )
        .unwrap();
        let names_all: Vec<&str> = all.iter().map(|d| d.name.as_str()).collect();
        assert!(names_all.contains(&"chunk_markdown_embed_aware"));
        assert!(names_all.contains(&"internal_only_helper"));
    }

    /// Index `files` (path, source) under a temp project root and return the
    /// dead-code names for `opts`.
    fn dead_names(files: &[(&str, &str)], opts: DeadCodeOptions) -> Vec<String> {
        use super::super::index_file;
        let dir = tempfile::tempdir().unwrap();
        let conn = mem_conn();
        let pid = test_project(&conn);
        for (path, src) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, src).unwrap();
            index_file(&conn, pid, path, src, "rs").unwrap();
        }
        let opts = DeadCodeOptions {
            project_root: Some(dir.path().to_path_buf()),
            ..opts
        };
        find_dead_code(&conn, pid, &opts)
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    const MIXED_SRC: &str = "\
fn orphan() {}

#[test]
fn checks_thing() {}

struct S;

impl S {
    fn inherent_orphan(&self) {}
}

impl Visitor for S {
    fn visit_thing(&self) {}
}

#[cfg(test)]
fn gated_helper() {}

#[cfg(test)]
mod tests {
    fn helper_fn() {}
}
";

    /// #716: tests, `#[cfg(test)]` modules, test directories and trait-impl
    /// methods are not dead code by default.
    #[test]
    fn test_dead_code_skips_tests_and_trait_impls_by_default() {
        let files = [
            ("src/a.rs", MIXED_SRC),
            ("tests/integ.rs", "fn fixture_orphan() {}\n"),
            ("src/foo_tests.rs", "fn suite_orphan() {}\n"),
        ];
        let names = dead_names(&files, DeadCodeOptions::default());
        assert!(names.contains(&"orphan".to_string()), "{names:?}");
        assert!(names.contains(&"inherent_orphan".to_string()), "{names:?}");
        for skipped in [
            "checks_thing",
            "gated_helper",
            "helper_fn",
            "fixture_orphan",
            "suite_orphan",
            "visit_thing",
        ] {
            assert!(
                !names.contains(&skipped.to_string()),
                "{skipped}: {names:?}"
            );
        }

        // --include-tests brings tests back, but never trait-impl methods.
        let names = dead_names(
            &files,
            DeadCodeOptions {
                include_tests: true,
                ..Default::default()
            },
        );
        for back in [
            "checks_thing",
            "gated_helper",
            "fixture_orphan",
            "suite_orphan",
        ] {
            assert!(names.contains(&back.to_string()), "{back}: {names:?}");
        }
        assert!(!names.contains(&"visit_thing".to_string()));
    }

    /// Regression: an inherent impl whose methods contain `for` loops must
    /// not be mistaken for a trait impl.
    #[test]
    fn test_dead_code_inherent_impl_with_for_loop_still_flagged() {
        let src = "\
struct R;

impl R {
    fn spin(&self) {
        for _ in 0..3 {}
    }

    fn lonely(&self) {}
}
";
        let names = dead_names(&[("src/r.rs", src)], DeadCodeOptions::default());
        assert!(names.contains(&"spin".to_string()), "{names:?}");
        assert!(names.contains(&"lonely".to_string()), "{names:?}");
    }

    /// Regression: without the project root the source rules cannot run, but
    /// the path rule still applies.
    #[test]
    fn test_dead_code_without_root_keeps_path_rules() {
        use super::super::index_file;
        let conn = mem_conn();
        let pid = test_project(&conn);
        index_file(&conn, pid, "tests/x.rs", "fn in_tests() {}\n", "rs").unwrap();
        index_file(&conn, pid, "src/y.rs", "fn plain() {}\n", "rs").unwrap();
        let dead = find_dead_code(&conn, pid, &DeadCodeOptions::default()).unwrap();
        let names: Vec<&str> = dead.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["plain"]);
    }

    /// #716: `pub` is API surface only for a crate with a lib target; in a
    /// bin-only crate it is dead like any private fn. `pub(crate)` is never
    /// API surface.
    #[test]
    fn test_dead_code_pub_only_skipped_in_lib_crates() {
        let bin = [("src/main.rs", "pub fn only_in_bin() {}\nfn main() {}\n")];
        let names = dead_names(&bin, DeadCodeOptions::default());
        assert!(names.contains(&"only_in_bin".to_string()), "{names:?}");

        let lib = [
            (
                "src/lib.rs",
                "pub fn api_item() {}\npub(crate) fn crate_only() {}\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
        ];
        let names = dead_names(&lib, DeadCodeOptions::default());
        assert!(!names.contains(&"api_item".to_string()), "{names:?}");
        assert!(names.contains(&"crate_only".to_string()), "{names:?}");

        let names = dead_names(
            &lib,
            DeadCodeOptions {
                include_pub_api: true,
                ..Default::default()
            },
        );
        assert!(names.contains(&"api_item".to_string()), "{names:?}");
    }
}
