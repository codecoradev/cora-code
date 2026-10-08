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
#[derive(Debug, Clone, Default)]
pub struct AffectedOptions {
    /// Override for the substrings that identify a test file when scanning
    /// callers (CLI `--filter`). `None` uses [`DEFAULT_TEST_FILE_MARKERS`].
    pub test_file_markers: Option<Vec<String>>,
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

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Find test files affected by a set of changed source files.
///
/// Two strategies, unioned and returned sorted and deduplicated:
/// 1. symbols defined in the changed files whose callers live in test files;
/// 2. test files matching the naming conventions of [`test_name_candidates`].
pub fn find_affected_tests(
    conn: &Connection,
    project_id: i64,
    changed: &[String],
    opts: &AffectedOptions,
) -> anyhow::Result<Vec<String>> {
    let mut affected: BTreeSet<String> = BTreeSet::new();
    let default_markers: Vec<String> = DEFAULT_TEST_FILE_MARKERS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let markers = opts.test_file_markers.as_ref().unwrap_or(&default_markers);

    // Strategy 1: symbols in changed files -> caller files that are tests.
    let mut symbols: BTreeSet<String> = BTreeSet::new();
    for chunk in changed.chunks(SQL_CHUNK) {
        let sql = format!(
            "SELECT DISTINCT name FROM symbols WHERE file IN ({}) AND project_id = ?",
            placeholders(chunk.len())
        );
        let mut params: Vec<Box<dyn ToSql>> = chunk
            .iter()
            .map(|f| Box::new(f.clone()) as Box<dyn ToSql>)
            .collect();
        params.push(Box::new(project_id));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(as_refs(&params).as_slice(), |r| r.get::<_, String>(0))?;
        for r in rows {
            symbols.insert(r?);
        }
    }
    let symbols: Vec<String> = symbols.into_iter().collect();
    for chunk in symbols.chunks(SQL_CHUNK) {
        let sql = format!(
            "SELECT DISTINCT file FROM call_graph WHERE callee IN ({}) AND project_id = ?",
            placeholders(chunk.len())
        );
        let mut params: Vec<Box<dyn ToSql>> = chunk
            .iter()
            .map(|s| Box::new(s.clone()) as Box<dyn ToSql>)
            .collect();
        params.push(Box::new(project_id));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(as_refs(&params).as_slice(), |r| r.get::<_, String>(0))?;
        for r in rows {
            let file = r?;
            if markers.iter().any(|m| file.contains(m.as_str())) {
                affected.insert(file);
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
            affected.insert(r?);
        }
    }

    Ok(affected.into_iter().collect())
}

/// Caller-controlled dead-code flags (the config-derived part is added by
/// [`dead_code_options`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct DeadCodeFlags {
    pub include_tests: bool,
    pub include_pub_api: bool,
    pub min_lines: Option<u32>,
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
            find_dead_code(&conn, pid, cfg, flags)
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
            dead_code_options(&cfg, flags).entry_point_patterns,
            s(&["plugin_hook_*"])
        );
        let n = names(&cfg);
        assert!(!n.contains(&"plugin_hook_entry".to_string()));
        assert!(n.contains(&"orphan_fn".to_string()));
    }
}
