//! Symbol index engine — persistent SQLite-backed symbol store.
//!
//! Build, query, and maintain a symbol index for code intelligence.
//! Uses regex-based extraction (same approach as `engine/context/extraction.rs`)
//! stored in SQLite with FTS5 for fast full-text search.

#[cfg(feature = "tree-sitter")]
mod ast;
pub mod brain;
mod extract;
pub mod graph;
pub mod schema;
mod symbols;
pub mod vector;

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tracing::{debug, info};

#[allow(unused_imports)]
pub use graph::{CallEdge, CalleeResult, CallerResult, ImpactNode};
pub use symbols::{SearchResult, SymbolKind, SymbolQuery};

/// Open or create the **global** symbol index database.
///
/// All projects share a single SQLite database at `~/.codecora/cora-code/cora.db`.
/// Project isolation is handled via the `project_id` foreign key.
pub fn open_global_index() -> anyhow::Result<Connection> {
    crate::data_dir::ensure_data_dir()?;
    let db_path = crate::data_dir::graph_db_path();

    let conn = Connection::open(&db_path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;\
         PRAGMA foreign_keys=ON;\
         PRAGMA synchronous=NORMAL;\
         PRAGMA cache_size=-65536;\
         PRAGMA mmap_size=268435456;\
         PRAGMA temp_store=MEMORY;"
    )?;
    schema::run_migrations(&conn)?;

    debug!("Opened global index at {}", db_path.display());
    Ok(conn)
}
/// Resolve the `project_id` for a given root path, creating the project row if needed.
pub fn ensure_project(conn: &Connection, root: &Path) -> anyhow::Result<i64> {
    let root_str = root.to_string_lossy().to_string();
    schema::get_or_create_project(conn, &root_str)
}

/// Detect the project root by walking up from `start` looking for marker files.
///
/// Search order: `.cora.yaml` → `Cargo.toml` → `package.json` → `.git` (dir or file).
/// Returns the directory containing the first marker found, or `None` if none is found.
pub fn resolve_project_root(start: &Path) -> Option<std::path::PathBuf> {
    let dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };

    const MARKERS: &[&str] = &[".cora.yaml", "Cargo.toml", "package.json", ".git"];

    let mut current = dir.to_path_buf();
    loop {
        for marker in MARKERS {
            let candidate = current.join(marker);
            if candidate.exists() {
                debug!(root = %current.display(), marker, "detected project root");
                return Some(current);
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Resolve `project_id` from the current directory, using project root detection.
///
/// Walks up from CWD to find a project root (`.cora.yaml`, `Cargo.toml`, etc.).
/// Falls back to CWD if no marker is found.
pub fn resolve_project_id(conn: &Connection) -> anyhow::Result<(i64, std::path::PathBuf)> {
    let cwd = std::env::current_dir()?;
    let root = resolve_project_root(&cwd).unwrap_or_else(|| cwd.clone());
    let project_id = ensure_project(conn, &root)?;
    Ok((project_id, root))
}

/// Index a single file: extract symbols and store in the database.
///
/// `project_id` is written to every row so data is scoped per-project
/// in the global database.
///
/// Returns the number of symbols indexed.
pub fn index_file(
    conn: &Connection,
    project_id: i64,
    file_path: &str,
    content: &str,
    language: &str,
) -> anyhow::Result<usize> {
    let fingerprint = file_fingerprint(content);
    let extracted = extract::extract_all(content, language, file_path);

    let tx = conn.unchecked_transaction()?;
    let count = index_file_in_tx(&tx, project_id, file_path, &fingerprint, language, &extracted)?;
    tx.commit()?;

    debug!(
        "Indexed {file_path}: {count} symbols, {} call edges ({language})",
        extracted.calls.len()
    );
    Ok(count)
}

/// Write extracted data for a single file within an existing transaction.
/// Does NOT commit — caller is responsible for `tx.commit()`.
fn index_file_in_tx(
    tx: &rusqlite::Transaction,
    project_id: i64,
    file_path: &str,
    fingerprint: &str,
    language: &str,
    extracted: &extract::ExtractedAll,
) -> anyhow::Result<usize> {
    // Delete existing symbols for this file within this project
    tx.execute(
        "DELETE FROM symbols WHERE file = ?1 AND project_id = ?2",
        rusqlite::params![file_path, project_id],
    )?;

    // Upsert file fingerprint
    tx.execute(
        "INSERT INTO files (path, fingerprint, last_indexed, language, symbol_count, project_id)
         VALUES (?1, ?2, datetime('now'), ?3, ?4, ?5)
         ON CONFLICT(path) DO UPDATE SET
           fingerprint = excluded.fingerprint,
           last_indexed = excluded.last_indexed,
           language = excluded.language,
           symbol_count = excluded.symbol_count,
           project_id = excluded.project_id",
        rusqlite::params![
            file_path,
            fingerprint,
            language,
            extracted.symbols.len() as i64,
            project_id
        ],
    )?;

    // Batch INSERT symbols using prepared statement
    let count = extracted.symbols.len();
    if !extracted.symbols.is_empty() {
        let mut stmt = tx.prepare(
            "INSERT INTO symbols (name, kind, file, line, signature, language, project_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        )?;
        for sym in &extracted.symbols {
            stmt.execute(rusqlite::params![
                sym.name,
                sym.kind.as_str(),
                sym.file,
                sym.line as i64,
                sym.signature,
                language,
                project_id,
            ])?;
        }
    }

    // Clear and insert call graph edges
    tx.execute(
        "DELETE FROM call_graph WHERE file = ?1 AND project_id = ?2",
        rusqlite::params![file_path, project_id],
    )?;
    if !extracted.calls.is_empty() {
        let mut stmt = tx.prepare(
            "INSERT INTO call_graph (caller, callee, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5)"
        )?;
        for site in &extracted.calls {
            stmt.execute(rusqlite::params![
                site.caller,
                site.callee,
                site.file,
                site.line as i64,
                project_id,
            ])?;
        }
    }

    // Clear and insert knowledge graph edges (tree-sitter only)
    #[cfg(feature = "tree-sitter")]
    {
        tx.execute(
            "DELETE FROM edges WHERE file = ?1 AND project_id = ?2",
            rusqlite::params![file_path, project_id],
        )?;
        if !extracted.kg_edges.is_empty() {
            let mut stmt = tx.prepare(
                "INSERT INTO edges (source, kind, target, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            )?;
            for e in &extracted.kg_edges {
                stmt.execute(rusqlite::params![
                    e.source,
                    e.kind.as_str(),
                    e.target,
                    e.file,
                    e.line as i64,
                    project_id,
                ])?;
            }
        }
    }

    Ok(count)
}

/// Check if a file needs re-indexing based on content hash.
pub fn needs_reindex(conn: &Connection, project_id: i64, file_path: &str, content: &str) -> bool {
    let fingerprint = file_fingerprint(content);

    let stored: Option<String> = conn
        .query_row(
            "SELECT fingerprint FROM files WHERE path = ?1 AND project_id = ?2",
            rusqlite::params![file_path, project_id],
            |row| row.get(0),
        )
        .ok();

    match stored {
        Some(fp) => fp != fingerprint,
        None => true,
    }
}

/// Index a project directory, respecting .gitignore.
///
/// Returns summary stats.
pub fn index_project(conn: &Connection, root: &Path, verbose: bool) -> anyhow::Result<IndexStats> {
    let project_id = ensure_project(conn, root)?;
    index_project_with_id(conn, project_id, root, verbose)
}

/// Internal: index a project with an already-resolved `project_id`.
fn index_project_with_id(
    conn: &Connection,
    project_id: i64,
    root: &Path,
    verbose: bool,
) -> anyhow::Result<IndexStats> {
    let mut stats = IndexStats::default();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .build();

    // Collect files to index
    let mut files_to_index: Vec<(String, String, String)> = Vec::new(); // (rel_str, content, language)
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().to_string();

        let language = crate::engine::diff_parser::detect_language(&rel_str);
        if language == "unknown" || language == "text" {
            continue;
        }

        stats.files_scanned += 1;

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !needs_reindex(conn, project_id, &rel_str, &content) {
            stats.files_skipped += 1;
            continue;
        }

        files_to_index.push((rel_str, content, language.to_string()));
    }

    if !files_to_index.is_empty() {
        // Single transaction for ALL files — eliminates per-file fsync
        let tx = conn.unchecked_transaction()?;

        // Disable FTS5 triggers during bulk insert to avoid 3x write amplification.
        // For a content-sync FTS5 table (content='symbols'), the correct bulk-load
        // sequence is: drop triggers → insert rows → 'rebuild' command → recreate triggers.
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS symbols_fts_insert;\
             DROP TRIGGER IF EXISTS symbols_fts_delete;\
             DROP TRIGGER IF EXISTS symbols_fts_update;"
        )?;

        for (rel_str, content, language) in &files_to_index {
            let fingerprint = file_fingerprint(content);
            let extracted = extract::extract_all(content, language, rel_str);
            match index_file_in_tx(&tx, project_id, rel_str, &fingerprint, language, &extracted) {
                Ok(n) => {
                    stats.files_indexed += 1;
                    stats.symbols_indexed += n;
                }
                Err(e) => {
                    stats.errors += 1;
                    if verbose {
                        eprintln!("  ⚠ Failed to index {rel_str}: {e}");
                    }
                }
            }
        }

        // Rebuild FTS5 index from the content table in one shot.
        // 'rebuild' tells FTS5 to discard its data and re-read from the
        // external content table (symbols) — much cheaper than per-row triggers.
        tx.execute("INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')", [])?;

        // Recreate FTS5 triggers for incremental updates after bulk load
        tx.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS symbols_fts_insert
             AFTER INSERT ON symbols
             BEGIN
                 INSERT INTO symbols_fts(rowid, name, signature)
                 VALUES (new.id, new.name, new.signature);
             END;

             CREATE TRIGGER IF NOT EXISTS symbols_fts_delete
             AFTER DELETE ON symbols
             BEGIN
                 INSERT INTO symbols_fts(symbols_fts, rowid, name, signature)
                 VALUES ('delete', old.id, old.name, old.signature);
             END;

             CREATE TRIGGER IF NOT EXISTS symbols_fts_update
             AFTER UPDATE ON symbols
             BEGIN
                 INSERT INTO symbols_fts(symbols_fts, rowid, name, signature)
                 VALUES ('delete', old.id, old.name, old.signature);
                 INSERT INTO symbols_fts(rowid, name, signature)
                 VALUES (new.id, new.name, new.signature);
             END;"
        )?;

        tx.commit()?;
    }

    // Update project's last_indexed timestamp
    conn.execute(
        "UPDATE projects SET last_indexed = datetime('now') WHERE id = ?1",
        rusqlite::params![project_id],
    )?;

    info!(
        "Index complete: {} files scanned, {} indexed, {} symbols, {} errors",
        stats.files_scanned, stats.files_indexed, stats.symbols_indexed, stats.errors
    );

    // Embed symbols into vector index for brain search
    match brain::embed_project(conn, project_id) {
        Ok(n) => {
            stats.embedded_symbols = Some(n);
            info!("Brain: embedded {n} symbols");
        }
        Err(e) => {
            if verbose {
                eprintln!("  ⚠ Embedding failed (non-fatal): {e}");
            }
            tracing::warn!("Embedding failed: {e}");
        }
    }

    Ok(stats)
}

/// Search the symbol index using FTS5 full-text search, scoped to a project.
pub fn search(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
) -> anyhow::Result<Vec<SearchResult>> {
    symbols::search(conn, project_id, query)
}

/// Get index statistics for a specific project.
pub fn index_stats(conn: &Connection, project_id: i64) -> anyhow::Result<IndexSummary> {
    let total_symbols: i64 = conn.query_row(
        "SELECT COUNT(*) FROM symbols WHERE project_id = ?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let total_files: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE project_id = ?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let db_size: i64 = {
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap_or(4096);
        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap_or(0);
        page_size * page_count
    };

    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut stmt =
        conn.prepare("SELECT kind, COUNT(*) FROM symbols WHERE project_id = ?1 GROUP BY kind")?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    for row in rows {
        let (kind, count) = row?;
        kind_counts.insert(kind, count);
    }

    let mut lang_counts: HashMap<String, usize> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT language, COUNT(*) FROM symbols WHERE project_id = ?1 GROUP BY language",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    for row in rows {
        let (lang, count) = row?;
        lang_counts.insert(lang, count);
    }

    Ok(IndexSummary {
        total_symbols: total_symbols as usize,
        total_files: total_files as usize,
        db_size_bytes: db_size as u64,
        symbols_by_kind: kind_counts,
        symbols_by_language: lang_counts,
    })
}

/// Remove symbols for files that no longer exist on disk, scoped to a project.
pub fn prune_deleted(conn: &Connection, project_id: i64, root: &Path) -> anyhow::Result<usize> {
    let mut deleted = 0;

    let mut stmt = conn.prepare("SELECT path FROM files WHERE project_id = ?1")?;
    let paths: Vec<String> = stmt
        .query_map(rusqlite::params![project_id], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let to_prune: Vec<&String> = paths
        .iter()
        .filter(|path| !root.join(path).exists())
        .collect();

    if !to_prune.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for path in &to_prune {
            tx.execute(
                "DELETE FROM symbols WHERE file = ?1 AND project_id = ?2",
                rusqlite::params![path, project_id],
            )?;
            tx.execute(
                "DELETE FROM call_graph WHERE file = ?1 AND project_id = ?2",
                rusqlite::params![path, project_id],
            )?;
            tx.execute(
                "DELETE FROM files WHERE path = ?1 AND project_id = ?2",
                rusqlite::params![path, project_id],
            )?;
        }
        tx.commit()?;
        deleted = to_prune.len();
    }

    if deleted > 0 {
        info!("Pruned {deleted} deleted files from index");
    }

    Ok(deleted)
}

/// Compute a SHA-256 fingerprint for file content.
fn file_fingerprint(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Statistics from an index build run.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub symbols_indexed: usize,
    pub errors: usize,
    pub embedded_symbols: Option<usize>,
}

/// Summary of the current index state.
#[derive(Debug, Clone)]
pub struct IndexSummary {
    pub total_symbols: usize,
    pub total_files: usize,
    pub db_size_bytes: u64,
    pub symbols_by_kind: HashMap<String, usize>,
    pub symbols_by_language: HashMap<String, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        schema::run_migrations(&conn).unwrap();
        conn
    }

    /// Create a test project and return its project_id.
    fn test_project(conn: &Connection) -> i64 {
        schema::get_or_create_project(conn, "/tmp/test-project").unwrap()
    }

    #[test]
    fn test_open_and_migrate() {
        let conn = mem_conn();
        // Tables should exist
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_index_rust_file() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        let code = r#"
use std::collections::HashMap;

pub struct Cache {
    inner: HashMap<String, String>,
}

impl Cache {
    pub fn new() -> Self {
        Self { inner: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.inner.get(key)
    }
}
"#;
        let count = index_file(&conn, project_id, "src/cache.rs", code, "rs").unwrap();
        assert!(count > 0, "Should extract symbols from Rust code");
    }

    #[test]
    fn test_needs_reindex() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        let code = "fn hello() {}";

        // First time → needs reindex
        assert!(needs_reindex(&conn, project_id, "test.rs", code));

        // Index it
        index_file(&conn, project_id, "test.rs", code, "rs").unwrap();

        // Same content → no reindex needed
        assert!(!needs_reindex(&conn, project_id, "test.rs", code));

        // Changed content → needs reindex
        assert!(needs_reindex(&conn, project_id, "test.rs", "fn world() {}"));
    }

    #[test]
    fn test_search() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        let code = r#"
pub fn authenticate(token: &str) -> bool {
    false
}

pub struct AuthService {
    secret: String,
}
"#;
        index_file(&conn, project_id, "src/auth.rs", code, "rs").unwrap();

        let query = SymbolQuery::text("authenticate");
        let results = search(&conn, project_id, &query).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].symbol.name.contains("authenticate"));
    }

    #[test]
    fn test_index_stats() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        index_file(&conn, project_id, "a.rs", "fn foo() {}", "rs").unwrap();
        index_file(&conn, project_id, "b.rs", "struct Bar {}", "rs").unwrap();

        let stats = index_stats(&conn, project_id).unwrap();
        assert!(stats.total_symbols >= 2);
        assert_eq!(stats.total_files, 2);
        assert!(stats.symbols_by_kind.contains_key("function"));
        assert!(stats.symbols_by_kind.contains_key("struct"));
    }

    #[test]
    fn test_prune_deleted() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        index_file(&conn, project_id, "gone.rs", "fn removed() {}", "rs").unwrap();

        // Create temp dir to use as root
        let tmp = tempfile::tempdir().unwrap();
        // gone.rs doesn't exist in temp dir → should be pruned
        let deleted = prune_deleted(&conn, project_id, tmp.path()).unwrap();
        assert_eq!(deleted, 1);

        let stats = index_stats(&conn, project_id).unwrap();
        assert_eq!(stats.total_symbols, 0);
    }

    #[test]
    fn test_reindex_replaces_symbols() {
        let conn = mem_conn();
        let project_id = test_project(&conn);
        index_file(&conn, project_id, "test.rs", "fn old_name() {}", "rs").unwrap();
        index_file(&conn, project_id, "test.rs", "fn new_name() {}", "rs").unwrap();

        let stats = index_stats(&conn, project_id).unwrap();
        // Should have 1 symbol (replaced, not 2)
        assert_eq!(stats.total_symbols, 1);
    }

    #[test]
    fn test_resolve_project_root_finds_cargo_toml() {
        // CWD of the test process is the crate root — Cargo.toml exists here.
        let cwd = std::env::current_dir().unwrap();
        let root = resolve_project_root(&cwd);
        assert!(root.is_some(), "should find project root from CWD");
        let root = root.unwrap();
        assert!(
            root.join("Cargo.toml").exists(),
            "resolved root should contain Cargo.toml"
        );
    }

    #[test]
    fn test_resolve_project_root_finds_cora_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join(".cora.yaml"), "version: 1\n").unwrap();
        let subdir = root.join("src").join("deep").join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        let resolved = resolve_project_root(&subdir);
        assert_eq!(resolved, Some(root));
    }

    #[test]
    fn test_resolve_project_root_returns_none_in_tmp() {
        // /tmp itself should have no markers (or at least a very deep /tmp subdirectory
        // that we create and then check).
        let tmp = tempfile::TempDir::new().unwrap();
        // Create a deep directory with no markers.
        let deep = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        // Walk up from `deep` — tempfile dirs are under /tmp which typically has no
        // Cargo.toml/.cora.yaml/.git. If it does find one we just skip this assertion.
        let resolved = resolve_project_root(&deep);
        // /tmp should not have project markers, but if the test machine has a git
        // repo at /tmp, we can't guarantee this. Only assert if /tmp is clean.
        if !std::path::Path::new("/tmp/.git").exists()
            && !std::path::Path::new("/tmp/Cargo.toml").exists()
            && !std::path::Path::new("/tmp/.cora.yaml").exists()
        {
            assert!(
                resolved.is_none(),
                "should not find project root in empty tmp dir, got {resolved:?}"
            );
        }
    }

    #[test]
    fn test_resolve_project_id_uses_project_root() {
        let conn = mem_conn();
        // resolve_project_id uses CWD — which is the cora-code crate root.
        let (pid, root) = resolve_project_id(&conn).unwrap();
        assert!(pid > 0);
        assert!(
            root.join("Cargo.toml").exists(),
            "resolved root should contain Cargo.toml"
        );
    }
}
