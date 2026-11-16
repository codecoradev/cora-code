//! Shared index queries used by both the CLI and the MCP server.
//!
//! The frontends (`cora affected`, `cora dead-code`, `cora.find_affected_tests`,
//! `cora.dead_code`) only parse input and format output; the query logic lives
//! here so the two can never diverge again (#568).

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, ToSql};

use super::graph::{self, DeadCodeOptions, DeadCodeResult};

/// Max number of changed files accepted at an untrusted input boundary (MCP).
/// The query itself chunks its bind parameters and has no hard limit.
pub const MAX_AFFECTED_FILES: usize = 200;

/// Default substrings that mark a caller's file as a test file.
pub const DEFAULT_TEST_FILE_MARKERS: &[&str] = &["test", "spec", "_test", "_spec"];

/// File extensions the naming-convention strategy generates candidates for.
const TEST_EXTENSIONS: &[&str] = &["rs", "go", "py", "ts", "tsx", "js", "jsx"];

/// Max bind parameters per statement chunk (SQLite's historical limit is 999).
const SQL_CHUNK: usize = 500;

/// Options for [`find_affected_tests`].
#[derive(Debug, Clone)]
pub struct AffectedOptions {
    /// Override for the substrings that identify a test file when scanning
    /// callers (CLI `--filter`). `None` uses [`DEFAULT_TEST_FILE_MARKERS`].
    pub test_file_markers: Option<Vec<String>>,
    /// Max call-graph hops from a changed symbol to a test (CLI `--depth`).
    pub depth: u32,
}

impl Default for AffectedOptions {
    fn default() -> Self {
        Self {
            test_file_markers: None,
            depth: DEFAULT_AFFECTED_DEPTH,
        }
    }
}

/// Default `--depth` of `cora affected`.
pub const DEFAULT_AFFECTED_DEPTH: u32 = 3;

/// A test file reached from a changed file, with why it was included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedHit {
    pub file: String,
    /// Call-graph hops (0 for a naming-convention match).
    pub depth: u32,
    /// Shortest chain from a changed symbol to the test, as `file::symbol`
    /// labels; a single `naming convention` entry for a name match.
    pub path: Vec<String>,
}

/// Module name a file is addressed by in a path qualifier
/// (`src/index/graph.rs` -> `graph`, `src/index/mod.rs` -> `index`).
fn module_name(file: &str) -> Option<&str> {
    let stem = file_stem(file)?;
    if stem == "mod" || stem == "lib" || stem == "main" {
        return Path::new(file)
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
    }
    Some(stem)
}

/// Whether a call edge reaches the symbol defined in `file` (impl type `ty`
/// when known). A name defined in a single file is resolved by name; a name
/// defined in several files only by an edge whose qualifier is the symbol's
/// type or module. An edge without qualifier (receiver type unknown) is not
/// followed then, so `x.new()` does not link every `new` in the project (#716).
fn edge_resolves(unique: bool, qualifier: Option<&str>, ty: Option<&str>, file: &str) -> bool {
    if unique {
        return true;
    }
    match qualifier {
        None => false,
        Some(q) => ty == Some(q) || module_name(file) == Some(q),
    }
}

/// Reject an oversized changed-file list (for untrusted callers such as MCP).
pub fn validate_changed_files(files: &[String]) -> anyhow::Result<()> {
    if files.len() > MAX_AFFECTED_FILES {
        anyhow::bail!(
            "Parameter 'files' has {} entries; maximum is {MAX_AFFECTED_FILES}",
            files.len()
        );
    }
    Ok(())
}

/// Escape `%`, `_` and the escape char itself for `LIKE ... ESCAPE '\'`.
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// File stem of a path (`src/foo.rs` -> `foo`, `a/foo.test.ts` -> `foo.test`).
/// `None` when the path has no usable stem.
pub fn file_stem(file: &str) -> Option<&str> {
    Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
}

/// Conventional test-file path suffixes for the given source files.
///
/// For every stem and every extension in `rs go py ts tsx js jsx` we emit:
/// `{stem}_test.{ext}`, `test_{stem}.{ext}`, `{stem}.test.{ext}`,
/// `{stem}.spec.{ext}`, `tests/{stem}.{ext}` and `__tests__/{stem}.{ext}`.
/// Candidates are matched as path suffixes, deduplicated and sorted.
pub fn test_name_candidates(files: &[String]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for stem in files.iter().filter_map(|f| file_stem(f)) {
        for ext in TEST_EXTENSIONS {
            names.extend([
                format!("{stem}_test.{ext}"),
                format!("test_{stem}.{ext}"),
                format!("{stem}.test.{ext}"),
                format!("{stem}.spec.{ext}"),
                format!("tests/{stem}.{ext}"),
                format!("__tests__/{stem}.{ext}"),
            ]);
        }
    }
    names.into_iter().collect()
}

fn as_refs(params: &[Box<dyn ToSql>]) -> Vec<&dyn ToSql> {
    params.iter().map(|p| p.as_ref()).collect()
}

/// Find test files affected by a set of changed source files.
///
/// Files only, sorted and deduplicated; see [`find_affected`] for the reason
/// each file is included.
pub fn find_affected_tests(
    conn: &Connection,
    project_id: i64,
    changed: &[String],
    opts: &AffectedOptions,
) -> anyhow::Result<Vec<String>> {
    let mut files: Vec<String> = find_affected(conn, project_id, changed, opts)?
        .into_iter()
        .map(|h| h.file)
        .collect();
    files.sort();
    Ok(files)
}

struct WalkNode {
    name: String,
    ty: Option<String>,
    file: String,
    /// A type defined in a changed file: reached by `Type::name` calls.
    is_type: bool,
}

type EdgeRow = (String, Option<String>, String, Option<String>);

fn label(file: &str, ty: Option<&str>, name: &str) -> String {
    match ty {
        Some(t) => format!("{file}::{t}::{name}"),
        None => format!("{file}::{name}"),
    }
}

/// Find test files affected by a set of changed source files.
///
/// Two strategies, unioned:
/// 1. a breadth-first walk up the call graph from the symbols defined in the
///    changed files, at most `opts.depth` hops, following only edges that
///    resolve to the symbol by type or module (see `edge_resolves`); every
///    caller living in a test file is a hit, recorded with the shortest path;
/// 2. test files matching the naming conventions of [`test_name_candidates`].
///
/// Sorted by depth, then file.
pub fn find_affected(
    conn: &Connection,
    project_id: i64,
    changed: &[String],
    opts: &AffectedOptions,
) -> anyhow::Result<Vec<AffectedHit>> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let mut hits: HashMap<String, AffectedHit> = HashMap::new();
    let default_markers: Vec<String> = DEFAULT_TEST_FILE_MARKERS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let markers = opts.test_file_markers.as_ref().unwrap_or(&default_markers);

    // Strategy 1 seeds: symbols of the changed files, with the impl type of
    // the ones that make calls (the call graph records it on the caller).
    let mut queue: VecDeque<(WalkNode, u32, Vec<String>)> = VecDeque::new();
    let mut seen: HashSet<(String, Option<String>, String)> = HashSet::new();
    for file in changed {
        let mut types: HashMap<String, Vec<String>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT caller, caller_type FROM call_graph
             WHERE file = ?1 AND project_id = ?2 AND caller_type IS NOT NULL",
        )?;
        for r in stmt.query_map(rusqlite::params![file, project_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (n, t) = r?;
            types.entry(n).or_default().push(t);
        }
        let mut stmt = conn.prepare(
            "SELECT DISTINCT name, kind FROM symbols WHERE file = ?1 AND project_id = ?2",
        )?;
        let syms: Vec<(String, String)> = stmt
            .query_map(rusqlite::params![file, project_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        for (name, kind) in syms {
            let is_type = matches!(
                kind.as_str(),
                "struct" | "enum" | "trait" | "class" | "interface" | "type_alias"
            );
            let tys: Vec<Option<String>> = match types.get(&name) {
                Some(v) => v.iter().cloned().map(Some).collect(),
                None => vec![None],
            };
            for ty in tys {
                if seen.insert((name.clone(), ty.clone(), file.clone())) {
                    let path = vec![label(file, ty.as_deref(), &name)];
                    queue.push_back((
                        WalkNode {
                            name: name.clone(),
                            ty,
                            file: file.clone(),
                            is_type,
                        },
                        0,
                        path,
                    ));
                }
            }
        }
    }

    let mut def_files: HashMap<String, bool> = HashMap::new();
    let mut is_unique = |name: &str| -> anyhow::Result<bool> {
        if let Some(u) = def_files.get(name) {
            return Ok(*u);
        }
        let n: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT file) FROM symbols
             WHERE name = ?1 AND project_id = ?2
               AND kind IN ('function', 'method', 'procedure')",
            rusqlite::params![name, project_id],
            |r| r.get(0),
        )?;
        def_files.insert(name.to_string(), n <= 1);
        Ok(n <= 1)
    };

    while let Some((node, depth, path)) = queue.pop_front() {
        if depth >= opts.depth {
            continue;
        }
        let unique = is_unique(&node.name)?;
        let mut edges: Vec<EdgeRow> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT caller, caller_type, file, qualifier FROM call_graph
             WHERE callee = ?1 AND project_id = ?2 LIMIT 1000",
        )?;
        for r in stmt.query_map(rusqlite::params![node.name, project_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })? {
            let row: EdgeRow = r?;
            if edge_resolves(
                unique && !node.is_type,
                row.3.as_deref(),
                node.ty.as_deref(),
                &node.file,
            ) {
                edges.push(row);
            }
        }
        // A type is also reached by every call made through it (`Type::new()`).
        if node.is_type {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT caller, caller_type, file, qualifier FROM call_graph
                 WHERE qualifier = ?1 AND project_id = ?2 LIMIT 1000",
            )?;
            for r in stmt.query_map(rusqlite::params![node.name, project_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })? {
                edges.push(r?);
            }
        }
        for (caller, cty, file, _) in edges {
            let mut next_path = path.clone();
            next_path.push(label(&file, cty.as_deref(), &caller));
            if markers.iter().any(|m| file.contains(m.as_str())) {
                hits.entry(file.clone()).or_insert_with(|| AffectedHit {
                    file: file.clone(),
                    depth: depth + 1,
                    path: next_path.clone(),
                });
            }
            if seen.insert((caller.clone(), cty.clone(), file.clone())) {
                queue.push_back((
                    WalkNode {
                        name: caller,
                        ty: cty,
                        file,
                        is_type: false,
                    },
                    depth + 1,
                    next_path,
                ));
            }
        }
    }

    // Strategy 2: naming convention, batched LIKE with ESCAPE.
    let candidates = test_name_candidates(changed);
    for chunk in candidates.chunks(SQL_CHUNK) {
        let clause = (1..=chunk.len())
            .map(|i| format!("path LIKE '%' || ?{i} ESCAPE '\\'"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!(
            "SELECT DISTINCT path FROM files WHERE ({clause}) AND project_id = ?{}",
            chunk.len() + 1
        );
        let mut params: Vec<Box<dyn ToSql>> = chunk
            .iter()
            .map(|t| Box::new(escape_like(t)) as Box<dyn ToSql>)
            .collect();
        params.push(Box::new(project_id));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(as_refs(&params).as_slice(), |r| r.get::<_, String>(0))?;
        for r in rows {
            let file = r?;
            hits.entry(file.clone()).or_insert_with(|| AffectedHit {
                file,
                depth: 0,
                path: vec!["naming convention".to_string()],
            });
        }
    }

    let mut out: Vec<AffectedHit> = hits.into_values().collect();
    out.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.file.cmp(&b.file)));
    Ok(out)
}

/// Caller-controlled dead-code flags (the config-derived part is added by
/// [`dead_code_options`]).
#[derive(Debug, Clone, Default)]
pub struct DeadCodeFlags {
    pub include_tests: bool,
    pub include_pub_api: bool,
    pub min_lines: Option<u32>,
    /// Project root, to read Rust sources for the test / trait-impl rules.
    pub project_root: Option<std::path::PathBuf>,
}

/// Build [`DeadCodeOptions`] from caller flags plus `analysis.entry_point_patterns`
/// of the given config. Both frontends go through here.
pub fn dead_code_options(
    config: &crate::config::schema::Config,
    flags: DeadCodeFlags,
) -> DeadCodeOptions {
    DeadCodeOptions {
        include_tests: flags.include_tests,
        min_lines: flags.min_lines,
        entry_point_patterns: config.analysis.entry_point_patterns.clone(),
        include_pub_api: flags.include_pub_api,
        project_root: flags.project_root,
    }
}

/// Find potentially dead symbols using config-derived entry-point patterns.
pub fn find_dead_code(
    conn: &Connection,
    project_id: i64,
    config: &crate::config::schema::Config,
    flags: DeadCodeFlags,
) -> anyhow::Result<Vec<DeadCodeResult>> {
    graph::find_dead_code(conn, project_id, &dead_code_options(config, flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::graph::{CallEdge, store_edges};

    fn setup() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/q").unwrap();
        (conn, pid)
    }

    fn add_file(conn: &Connection, pid: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (project_id, path, fingerprint, last_indexed) VALUES (?1, ?2, 'f', 't')",
            rusqlite::params![pid, path],
        )
        .unwrap();
    }

    fn add_symbol(conn: &Connection, pid: i64, name: &str, file: &str) {
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, project_id) VALUES (?1, 'function', ?2, 1, ?3)",
            rusqlite::params![name, file, pid],
        )
        .unwrap();
    }

    fn call(conn: &Connection, pid: i64, callee: &str, file: &str) {
        store_edges(
            conn,
            &[CallEdge {
                caller: "caller_fn".into(),
                callee: callee.into(),
                file: file.into(),
                line: 1,
            }],
            pid,
        )
        .unwrap();
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn stem_is_not_the_extension() {
        assert_eq!(file_stem("foo.rs"), Some("foo"));
        assert_eq!(file_stem("foo.test.ts"), Some("foo.test"));
        assert_eq!(file_stem("dir/foo.py"), Some("foo"));
        assert_eq!(file_stem(""), None);
        assert_eq!(file_stem("/"), None);

        let names = test_name_candidates(&s(&["src/engine/review.rs"]));
        assert!(names.contains(&"review_test.rs".into()));
        assert!(names.contains(&"tests/review.rs".into()));
        assert!(names.contains(&"test_review.py".into()));
        assert!(names.contains(&"review.spec.tsx".into()));
        assert!(names.contains(&"__tests__/review.js".into()));
        assert!(names.contains(&"review.test.go".into()));
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("rs_") || n == "rs.test.ts")
        );
        assert!(test_name_candidates(&s(&["", "/"])).is_empty());
    }

    #[test]
    fn like_escaping() {
        assert_eq!(escape_like("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn like_wildcards_match_literally() {
        let (conn, pid) = setup();
        add_file(&conn, pid, "tests/a_b_test.rs");
        add_file(&conn, pid, "tests/axb_test.rs");
        add_file(&conn, pid, "tests/a%b_test.rs");
        add_file(&conn, pid, "tests/azzb_test.rs");
        let d = AffectedOptions::default();
        let got = find_affected_tests(&conn, pid, &s(&["src/a_b.rs"]), &d).unwrap();
        assert_eq!(got, s(&["tests/a_b_test.rs"]));
        let got = find_affected_tests(&conn, pid, &s(&["src/a%b.rs"]), &d).unwrap();
        assert_eq!(got, s(&["tests/a%b_test.rs"]));
    }

    #[test]
    fn naming_convention_strategy() {
        let (conn, pid) = setup();
        for p in [
            "src/foo_test.go",
            "tests/foo.rs",
            "pkg/test_foo.py",
            "web/foo.test.ts",
            "web/foo.spec.tsx",
            "web/__tests__/foo.js",
            "src/foo.rs",
            "src/other_test.rs",
        ] {
            add_file(&conn, pid, p);
        }
        let d = AffectedOptions::default();
        let got = find_affected_tests(&conn, pid, &s(&["src/foo.rs"]), &d).unwrap();
        assert_eq!(
            got,
            s(&[
                "pkg/test_foo.py",
                "src/foo_test.go",
                "tests/foo.rs",
                "web/__tests__/foo.js",
                "web/foo.spec.tsx",
                "web/foo.test.ts",
            ])
        );
    }

    #[test]
    fn caller_strategy_and_filter() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "do_work", "src/lib.rs");
        call(&conn, pid, "do_work", "tests/integration.rs");
        call(&conn, pid, "do_work", "src/main.rs");
        let d = AffectedOptions::default();
        let got = find_affected_tests(&conn, pid, &s(&["src/lib.rs"]), &d).unwrap();
        assert_eq!(got, s(&["tests/integration.rs"]));

        let opts = AffectedOptions {
            test_file_markers: Some(s(&["main"])),
            ..Default::default()
        };
        let got = find_affected_tests(&conn, pid, &s(&["src/lib.rs"]), &opts).unwrap();
        assert_eq!(got, s(&["src/main.rs"]));
    }

    #[test]
    fn results_are_project_scoped() {
        let (conn, pid) = setup();
        let other = crate::index::schema::get_or_create_project(&conn, "/tmp/other").unwrap();
        add_file(&conn, other, "tests/foo.rs");
        let d = AffectedOptions::default();
        let got = find_affected_tests(&conn, pid, &s(&["src/foo.rs"]), &d).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn many_files_are_chunked() {
        let (conn, pid) = setup();
        let files: Vec<String> = (0..1200).map(|i| format!("src/m{i}.rs")).collect();
        add_file(&conn, pid, "tests/m1199.rs");
        let got = find_affected_tests(&conn, pid, &files, &AffectedOptions::default()).unwrap();
        assert_eq!(got, s(&["tests/m1199.rs"]));
    }

    #[test]
    fn cap_enforced() {
        let ok: Vec<String> = (0..MAX_AFFECTED_FILES)
            .map(|i| format!("f{i}.rs"))
            .collect();
        assert!(validate_changed_files(&ok).is_ok());
        let too_many: Vec<String> = (0..=MAX_AFFECTED_FILES)
            .map(|i| format!("f{i}.rs"))
            .collect();
        let err = validate_changed_files(&too_many).unwrap_err().to_string();
        assert!(err.contains("maximum is 200"));
    }

    #[test]
    fn dead_code_honors_entry_point_patterns() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "plugin_hook_entry", "src/a.rs");
        add_symbol(&conn, pid, "orphan_fn", "src/a.rs");
        let flags = DeadCodeFlags {
            include_pub_api: true,
            ..Default::default()
        };
        let names = |cfg: &crate::config::schema::Config| -> Vec<String> {
            find_dead_code(&conn, pid, cfg, flags.clone())
                .unwrap()
                .into_iter()
                .map(|r| r.name)
                .collect()
        };

        let plain = crate::config::schema::Config::default();
        let n = names(&plain);
        assert!(n.contains(&"plugin_hook_entry".to_string()));
        assert!(n.contains(&"orphan_fn".to_string()));

        let mut cfg = crate::config::schema::Config::default();
        cfg.analysis.entry_point_patterns = s(&["plugin_hook_*"]);
        assert_eq!(
            dead_code_options(&cfg, flags.clone()).entry_point_patterns,
            s(&["plugin_hook_*"])
        );
        let n = names(&cfg);
        assert!(!n.contains(&"plugin_hook_entry".to_string()));
        assert!(n.contains(&"orphan_fn".to_string()));
    }

    fn edge(conn: &Connection, pid: i64, caller: &str, callee: &str, file: &str, q: Option<&str>) {
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id, qualifier)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            rusqlite::params![caller, callee, file, pid, q],
        )
        .unwrap();
    }

    fn affected(conn: &Connection, pid: i64, changed: &str, depth: u32) -> Vec<AffectedHit> {
        let opts = AffectedOptions {
            depth,
            ..Default::default()
        };
        find_affected(conn, pid, &s(&[changed]), &opts).unwrap()
    }

    /// #716: the walk follows callers transitively up to `--depth`, and each
    /// hit carries the shortest edge path.
    #[test]
    fn depth_limit_and_shortest_path() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "leaf", "src/a.rs");
        add_symbol(&conn, pid, "mid", "src/b.rs");
        add_symbol(&conn, pid, "top", "src/c.rs");
        edge(&conn, pid, "mid", "leaf", "src/b.rs", None);
        edge(&conn, pid, "top", "mid", "src/c.rs", None);
        edge(&conn, pid, "it_works", "top", "tests/it.rs", None);
        // A shorter route to the same test must win over the long one.
        edge(&conn, pid, "it_works", "leaf", "tests/it.rs", None);

        let hits = affected(&conn, pid, "src/a.rs", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "tests/it.rs");
        assert_eq!(hits[0].depth, 1);
        assert_eq!(
            hits[0].path,
            s(&["src/a.rs::leaf", "tests/it.rs::it_works"])
        );

        // Without the direct edge the test is three hops away.
        conn.execute(
            "DELETE FROM call_graph WHERE callee = 'leaf' AND file = 'tests/it.rs'",
            [],
        )
        .unwrap();
        let hits = affected(&conn, pid, "src/a.rs", 3);
        assert_eq!(hits[0].depth, 3);
        assert_eq!(
            hits[0].path,
            s(&[
                "src/a.rs::leaf",
                "src/b.rs::mid",
                "src/c.rs::top",
                "tests/it.rs::it_works"
            ])
        );
        assert!(affected(&conn, pid, "src/a.rs", 2).is_empty());
        assert!(affected(&conn, pid, "src/a.rs", 0).is_empty());
    }

    /// Regression: call cycles terminate.
    #[test]
    fn cycles_terminate() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "ping", "src/a.rs");
        add_symbol(&conn, pid, "pong", "src/b.rs");
        edge(&conn, pid, "pong", "ping", "src/b.rs", None);
        edge(&conn, pid, "ping", "pong", "src/a.rs", None);
        edge(&conn, pid, "t", "pong", "tests/c.rs", None);
        assert_eq!(affected(&conn, pid, "src/a.rs", 10).len(), 1);
    }

    /// Regression (#716): an unqualified call to a name defined in several
    /// files (`new`) must not link a test to every definition.
    #[test]
    fn ambiguous_names_need_a_resolving_qualifier() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "new", "src/a.rs");
        add_symbol(&conn, pid, "new", "src/b.rs");
        edge(&conn, pid, "t1", "new", "tests/x.rs", None);
        assert!(affected(&conn, pid, "src/a.rs", 3).is_empty());
        assert!(affected(&conn, pid, "src/b.rs", 3).is_empty());

        // Reached through the module `a`: only a.rs is affected.
        edge(&conn, pid, "t2", "new", "tests/y.rs", Some("a"));
        let hits = affected(&conn, pid, "src/a.rs", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "tests/y.rs");
        assert!(affected(&conn, pid, "src/b.rs", 3).is_empty());
    }

    /// Regression (#716): two types with the same method name stay apart;
    /// the caller type recorded in the changed file picks the right edges.
    #[test]
    fn type_qualifier_selects_the_right_impl() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "from_env", "src/a.rs");
        add_symbol(&conn, pid, "from_env", "src/b.rs");
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id, caller_type)
             VALUES ('from_env', 'helper', 'src/a.rs', 1, ?1, 'Alpha')",
            [pid],
        )
        .unwrap();
        edge(&conn, pid, "t", "from_env", "tests/x.rs", Some("Alpha"));
        edge(&conn, pid, "u", "from_env", "tests/z.rs", Some("Beta"));

        let hits = affected(&conn, pid, "src/a.rs", 3);
        assert_eq!(
            hits.iter().map(|h| h.file.as_str()).collect::<Vec<_>>(),
            vec!["tests/x.rs"]
        );
    }

    /// A type defined in a changed file is reached by `Type::method()` calls
    /// even when the method itself is ambiguous.
    #[test]
    fn changed_type_is_reached_through_its_qualifier() {
        let (conn, pid) = setup();
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, project_id)
             VALUES ('Alpha', 'struct', 'src/a.rs', 1, ?1)",
            [pid],
        )
        .unwrap();
        add_symbol(&conn, pid, "new", "src/a.rs");
        add_symbol(&conn, pid, "new", "src/b.rs");
        edge(&conn, pid, "t", "new", "tests/x.rs", Some("Alpha"));
        let hits = affected(&conn, pid, "src/a.rs", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "tests/x.rs");
    }

    #[test]
    fn naming_hits_have_depth_zero_and_sort_first() {
        let (conn, pid) = setup();
        add_symbol(&conn, pid, "work", "src/foo.rs");
        add_file(&conn, pid, "tests/foo.rs");
        edge(&conn, pid, "t", "work", "tests/aaa.rs", None);
        let hits = affected(&conn, pid, "src/foo.rs", 3);
        assert_eq!(hits[0].file, "tests/foo.rs");
        assert_eq!(hits[0].depth, 0);
        assert_eq!(hits[0].path, s(&["naming convention"]));
        assert_eq!(hits[1].file, "tests/aaa.rs");
    }
}
