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

use rayon::prelude::*;
use rusqlite::Connection;
#[cfg(test)]
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
         PRAGMA temp_store=MEMORY;",
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
/// Resolution order:
/// 1. `.cora.yaml` — explicit user override, always wins immediately.
/// 2. A `Cargo.toml` declaring a `[workspace]` section — a Rust workspace root
///    beats a nested member crate's plain `Cargo.toml`, so indexing from inside
///    `crates/*` and resolving from the repo root land on the same project (#522).
/// 3. The first plain marker (`Cargo.toml`, `package.json`, `.git`) as fallback.
///
/// The walk never climbs past a git repository boundary, so an unrelated
/// `[workspace]` outside the repo cannot hijack resolution.
pub fn resolve_project_root(start: &Path) -> Option<std::path::PathBuf> {
    let dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };

    let mut current = dir.to_path_buf();
    let mut fallback: Option<std::path::PathBuf> = None;
    loop {
        // 1. Explicit cora config wins immediately.
        if current.join(".cora.yaml").is_file() {
            debug!(root = %current.display(), marker = ".cora.yaml", "detected project root");
            return Some(current);
        }

        // 2. Cargo workspace root beats a nested member crate manifest.
        let cargo_toml = current.join("Cargo.toml");
        if cargo_toml.is_file()
            && std::fs::read_to_string(&cargo_toml)
                .map(|s| s.contains("[workspace"))
                .unwrap_or(false)
        {
            debug!(root = %current.display(), marker = "[workspace] Cargo.toml", "detected project root");
            return Some(current);
        }

        // 3. First plain marker is the fallback (original behavior).
        if fallback.is_none() {
            const MARKERS: &[&str] = &["Cargo.toml", "package.json", ".git"];
            for marker in MARKERS {
                if current.join(marker).exists() {
                    fallback = Some(current.clone());
                    break;
                }
            }
        }

        // Repo boundary: stop AFTER giving this directory its own chance to
        // match above (a repo root can legitimately be the workspace root).
        if current.join(".git").exists() {
            break;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }

    if let Some(root) = &fallback {
        debug!(root = %root.display(), "detected project root");
    }
    fallback
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

#[cfg(test)]
/// Index a single file: extract symbols and store in the database.
/// Test-only — production uses `index_project_with_id` with batch fingerprinting.
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
    let count = index_file_in_tx(
        &tx,
        project_id,
        file_path,
        &fingerprint,
        language,
        &extracted,
    )?;

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
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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

/// Load all file fingerprints for a project in a single query.
/// Returns `HashMap<file_path, fingerprint>` — O(1) lookup per file
/// instead of N individual DB roundtrips.
fn load_all_fingerprints(
    conn: &Connection,
    project_id: i64,
) -> anyhow::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT path, fingerprint FROM files WHERE project_id = ?1")?;
    let map: HashMap<String, String> = stmt
        .query_map([project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(map)
}

/// Index a project directory, respecting .gitignore.
///
/// Returns summary stats.
pub fn index_project(conn: &Connection, root: &Path, verbose: bool) -> anyhow::Result<IndexStats> {
    index_project_with_skip(conn, root, verbose, None)
}

/// Index a project directory, with config hash invalidation.
///
/// If `skip_patterns` is provided, the config hash is compared against
/// the stored hash in the DB. If they differ, all fingerprints are
/// cleared, forcing a full re-index.
pub fn index_project_with_skip(
    conn: &Connection,
    root: &Path,
    verbose: bool,
    skip_patterns: Option<&[String]>,
) -> anyhow::Result<IndexStats> {
    let project_id = ensure_project(conn, root)?;

    // Config hash invalidation: if skip_patterns changed, clear fingerprints
    if let Some(patterns) = skip_patterns {
        let new_hash = schema::compute_index_config_hash(patterns);
        let stored_hash: Option<String> = conn
            .query_row(
                "SELECT index_config_hash FROM projects WHERE id = ?1",
                rusqlite::params![project_id],
                |row| row.get(0),
            )
            .ok()
            .flatten();

        if stored_hash.as_deref() != Some(&new_hash) {
            if verbose {
                eprintln!("  ↻ Config changed — invalidating fingerprints for full re-index");
            }
            // Clear all fingerprints for this project to force re-index
            conn.execute(
                "DELETE FROM files WHERE project_id = ?1",
                rusqlite::params![project_id],
            )?;
            // Also clear symbols/edges so they're rebuilt from scratch
            conn.execute(
                "DELETE FROM symbols WHERE project_id = ?1",
                rusqlite::params![project_id],
            )?;
            conn.execute(
                "DELETE FROM edges WHERE project_id = ?1",
                rusqlite::params![project_id],
            )?;
            conn.execute(
                "DELETE FROM call_graph WHERE project_id = ?1",
                rusqlite::params![project_id],
            )?;
            // Store the new config hash
            conn.execute(
                "UPDATE projects SET index_config_hash = ?1 WHERE id = ?2",
                rusqlite::params![new_hash, project_id],
            )?;
        }
    }

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

    // Collect files to index
    let mut files_to_index: Vec<(String, String, String, String)> = Vec::new(); // (rel_str, content, language, cheap_fp)

    // Batch-load all fingerprints in ONE query — eliminates N per-file DB roundtrips.
    let stored_fingerprints = load_all_fingerprints(conn, project_id).unwrap_or_default();

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .build();

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

        // Compute mtime:size fingerprint — cheap, no file read needed.
        // metadata() is a stat() call, ~microseconds per file.
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let size = metadata.len();
        let cheap_fp = format!("{mtime}:{size}");

        // O(1) hashmap lookup — skip file entirely if unchanged
        if let Some(stored) = stored_fingerprints.get(&rel_str) {
            if *stored == cheap_fp {
                stats.files_skipped += 1;
                continue;
            }
        }

        // Only read file content when we know it changed
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_to_index.push((rel_str, content, language.to_string(), cheap_fp));
    }

    if !files_to_index.is_empty() {
        // ── Phase 1: Parallel extraction (CPU-bound tree-sitter + regex) ───
        // Rayon par_iter distributes file parsing across all CPU cores.
        // extract_all is CPU-bound (~4ms/file) and fully thread-safe.
        let t_extract = std::time::Instant::now();
        let extracted_files: Vec<(String, extract::ExtractedAll, String, String)> = files_to_index
            .par_iter()
            .map(|(rel_str, content, language, cheap_fp)| {
                let mut extracted = extract::extract_all(content, language, rel_str);
                // Detect HTTP route edges and append to kg_edges
                extracted
                    .kg_edges
                    .extend(extract::detect_routes(content, language, rel_str));
                (
                    rel_str.clone(),
                    extracted,
                    language.clone(),
                    cheap_fp.clone(),
                )
            })
            .collect();
        let extract_ms = t_extract.elapsed().as_millis();

        // ── Phase 2: Serial SQLite writes (single transaction) ─────────
        let t_db = std::time::Instant::now();
        let tx = conn.unchecked_transaction()?;

        // Disable FTS5 triggers during bulk insert to avoid 3x write amplification.
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS symbols_fts_insert;\
             DROP TRIGGER IF EXISTS symbols_fts_delete;\
             DROP TRIGGER IF EXISTS symbols_fts_update;",
        )?;

        for (rel_str, extracted, language, cheap_fp) in &extracted_files {
            match index_file_in_tx(&tx, project_id, rel_str, cheap_fp, language, extracted) {
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
                 INSERT INTO symbols_fts(rowid, name, signature, file)
                 VALUES (new.id, new.name, new.signature, new.file);
             END;

             CREATE TRIGGER IF NOT EXISTS symbols_fts_delete
             AFTER DELETE ON symbols
             BEGIN
                 INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, file)
                 VALUES ('delete', old.id, old.name, old.signature, old.file);
             END;

             CREATE TRIGGER IF NOT EXISTS symbols_fts_update
             AFTER UPDATE ON symbols
             BEGIN
                 INSERT INTO symbols_fts(symbols_fts, rowid, name, signature, file)
                 VALUES ('delete', old.id, old.name, old.signature, old.file);
                 INSERT INTO symbols_fts(rowid, name, signature, file)
                 VALUES (new.id, new.name, new.signature, new.file);
             END;",
        )?;

        tx.commit()?;
        tracing::debug!(
            "extract={}ms (rayon), db={}ms, files={}",
            extract_ms,
            t_db.elapsed().as_millis(),
            files_to_index.len()
        );
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

    // Embed symbols into vector index for brain search — only when files changed
    if stats.files_indexed > 0 {
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

#[cfg(test)]
/// Compute a SHA-256 fingerprint for file content (test-only).
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

    /// Regression (#522): running `cora index` from inside a workspace member
    /// crate must resolve to the WORKSPACE root (the member's plain
    /// `Cargo.toml` is not the project root), so CLI and MCP agree on one
    /// project_id instead of silently creating two.
    #[test]
    fn test_resolve_project_root_prefers_workspace_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let member = ws.join("crates").join("app");
        std::fs::create_dir_all(&member).unwrap();

        std::fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(member.join("src.rs"), "fn main() {}\n").unwrap();

        let resolved = resolve_project_root(&member);
        assert_eq!(
            resolved.as_deref(),
            Some(ws.as_path()),
            "workspace root should win over a member crate's plain Cargo.toml"
        );
    }

    /// An explicit `.cora.yaml` anywhere along the walk always wins — it is a
    /// deliberate user override of project-root detection.
    #[test]
    fn test_resolve_project_root_cora_yaml_wins_over_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let member = ws.join("crates").join("app");
        std::fs::create_dir_all(&member).unwrap();

        std::fs::write(
            ws.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        std::fs::write(ws.join(".cora.yaml"), "version: 1\n").unwrap();
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(member.join(".cora.yaml"), "version: 1\n").unwrap();

        let resolved = resolve_project_root(&member);
        assert_eq!(resolved.as_deref(), Some(member.as_path()));
    }

    /// Root detection must not climb above a git repository boundary: an
    /// unrelated `[workspace]` Cargo.toml outside the repo must never hijack
    /// resolution.
    #[test]
    fn test_resolve_project_root_stops_at_git_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let outer_ws = tmp.path().join("outer");
        let repo = outer_ws.join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        std::fs::write(
            outer_ws.join("Cargo.toml"),
            "[workspace]\nmembers = [\"*\"]\n",
        )
        .unwrap();
        // Repo itself has no markers other than .git and one plain file dir.
        let deep = repo.join("src");
        std::fs::create_dir_all(&deep).unwrap();

        let resolved = resolve_project_root(&deep);
        assert_eq!(
            resolved.as_deref(),
            Some(repo.as_path()),
            ".git must stop the upward walk"
        );
    }

    /// Regression (#522): an incremental no-op re-index (all fingerprints
    /// match) must keep reporting the STORED symbol count — DB state must
    /// survive untouched re-runs.
    #[test]
    fn test_incremental_index_preserves_counts() {
        let conn = mem_conn();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("lib.rs"), "pub fn alpha() {} pub fn beta() {}\n").unwrap();

        let first = index_project(&conn, &root, false).unwrap();
        assert_eq!(first.files_indexed, 1);
        assert!(first.symbols_indexed > 0);

        // Second run: everything unchanged → skipped, nothing wiped.
        let second = index_project(&conn, &root, false).unwrap();
        assert_eq!(second.files_skipped, 1);
        assert_eq!(second.files_indexed, 0);

        let summary = index_stats(&conn, ensure_project(&conn, &root).unwrap()).unwrap();
        assert_eq!(
            summary.total_symbols as usize, first.symbols_indexed,
            "stored symbols must survive an incremental no-op re-run"
        );
        assert_eq!(summary.total_files, 1);
    }
}
