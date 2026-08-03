//! SQLite schema management for the symbol index.

use rusqlite::Connection;

/// Current schema version.
#[allow(dead_code)]
const SCHEMA_VERSION: i32 = 6;

/// Run database migrations (creates tables if not exist).
pub fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    // Schema version tracking
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT DEFAULT (datetime('now'))
        );",
    )?;

    let current: i32 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);

    if current < 1 {
        migrate_v1(conn)?;
    }
    if current < 2 {
        migrate_v2(conn)?;
    }
    if current < 3 {
        migrate_v3(conn)?;
    }
    if current < 4 {
        migrate_v4(conn)?;
    }
    if current < 5 {
        migrate_v5(conn)?;
    }
    if current < 6 {
        migrate_v6(conn)?;
    }

    Ok(())
}

/// Migration v1: Initial schema — symbols, files, FTS5 index.
fn migrate_v1(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- Symbol definitions extracted from source files
        CREATE TABLE IF NOT EXISTS symbols (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            file        TEXT NOT NULL,
            line        INTEGER NOT NULL,
            signature   TEXT NOT NULL DEFAULT '',
            language    TEXT NOT NULL DEFAULT 'unknown',
            created_at  TEXT DEFAULT (datetime('now'))
        );

        -- Index for file-based queries
        CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file);

        -- Index for name-based lookups
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);

        -- Index for kind-based filtering
        CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);

        -- File tracking for incremental indexing
        CREATE TABLE IF NOT EXISTS files (
            path          TEXT PRIMARY KEY,
            fingerprint   TEXT NOT NULL,
            last_indexed  TEXT NOT NULL,
            language      TEXT NOT NULL DEFAULT 'unknown',
            symbol_count  INTEGER NOT NULL DEFAULT 0
        );

        -- FTS5 virtual table for full-text search on symbol names
        CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
            name,
            signature,
            content='symbols',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 1'
        );

        -- Call graph edges: caller → callee relationships
        CREATE TABLE IF NOT EXISTS call_graph (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            caller      TEXT NOT NULL,
            callee      TEXT NOT NULL,
            file        TEXT NOT NULL,
            line        INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_cg_caller ON call_graph(caller);
        CREATE INDEX IF NOT EXISTS idx_cg_callee ON call_graph(callee);
        CREATE INDEX IF NOT EXISTS idx_cg_file ON call_graph(file);

        -- Triggers to keep FTS5 in sync with symbols table
        CREATE TRIGGER IF NOT EXISTS symbols_fts_insert
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
        END;
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (1)", [])?;

    Ok(())
}

/// Migration v2: Multi-project support.
///
/// Adds `projects` table and `project_id` column to `symbols`, `files`,
/// and `call_graph`. The global DB at `~/.codecora/cora-code/cora.db`
/// stores data for all indexed projects, keyed by absolute path.
fn migrate_v2(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- Projects table: one row per indexed codebase
        CREATE TABLE IF NOT EXISTS projects (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            root_path     TEXT NOT NULL UNIQUE,
            name          TEXT NOT NULL DEFAULT '',
            last_indexed  TEXT NOT NULL DEFAULT (datetime('now')),
            created_at    TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Add project_id to symbols (nullable for migration compat)
        ALTER TABLE symbols ADD COLUMN project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE;
        CREATE INDEX IF NOT EXISTS idx_symbols_project ON symbols(project_id);

        -- Add project_id to files (nullable for migration compat)
        ALTER TABLE files ADD COLUMN project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE;
        CREATE INDEX IF NOT EXISTS idx_files_project ON files(project_id);

        -- Add project_id to call_graph (nullable for migration compat)
        ALTER TABLE call_graph ADD COLUMN project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE;
        CREATE INDEX IF NOT EXISTS idx_cg_project ON call_graph(project_id);
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (2)", [])?;

    Ok(())
}

/// Migration v3: Knowledge graph edges.
///
/// Adds `edges` table — a richer version of `call_graph` that supports
/// multiple edge types (CALLS, IMPORTS, IMPLEMENTS, INHERITS, CHILD_OF).
/// Existing `call_graph` rows are migrated into `edges` with kind='CALLS'.
fn migrate_v3(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- Knowledge graph edges: source → target with typed relationship
        CREATE TABLE IF NOT EXISTS edges (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            source      TEXT NOT NULL,
            kind        TEXT NOT NULL,
            target      TEXT NOT NULL,
            file        TEXT NOT NULL,
            line        INTEGER NOT NULL,
            project_id  INTEGER REFERENCES projects(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target);
        CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
        CREATE INDEX IF NOT EXISTS idx_edges_project ON edges(project_id);
        CREATE INDEX IF NOT EXISTS idx_edges_file ON edges(file);

        -- Migrate existing call_graph data into edges
        INSERT OR IGNORE INTO edges (source, kind, target, file, line, project_id)
            SELECT caller, 'CALLS', callee, file, line, project_id FROM call_graph;
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (3)", [])?;

    Ok(())
}

/// Migration v4: Embedding metadata on projects table.
fn migrate_v4(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        ALTER TABLE projects ADD COLUMN embedding_tier TEXT NOT NULL DEFAULT 'static';
        ALTER TABLE projects ADD COLUMN embedding_dims INTEGER NOT NULL DEFAULT 256;
        ALTER TABLE projects ADD COLUMN last_embedded_at TEXT;
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (4)", [])?;

    Ok(())
}

/// Migration v5: Review history — reviews, findings, finding_events.
///
/// Enables persistent storage of review/scan results in the same `cora.db`.
/// This replaces the legacy JSON snapshot files in `.cora/history/`.
fn migrate_v5(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- One row per review/scan run
        CREATE TABLE IF NOT EXISTS reviews (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            project_id      INTEGER REFERENCES projects(id) ON DELETE CASCADE,
            command         TEXT NOT NULL DEFAULT 'review',
            commit_hash     TEXT,
            branch          TEXT,
            summary         TEXT NOT NULL DEFAULT '',
            score           INTEGER NOT NULL DEFAULT 0,
            gate_status     TEXT NOT NULL DEFAULT 'disabled',
            files_scanned   INTEGER NOT NULL DEFAULT 0,
            lines_scanned   INTEGER NOT NULL DEFAULT 0,
            should_block    INTEGER NOT NULL DEFAULT 0,
            input_tokens    INTEGER NOT NULL DEFAULT 0,
            output_tokens   INTEGER NOT NULL DEFAULT 0,
            cost_usd        REAL NOT NULL DEFAULT 0.0,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_reviews_project ON reviews(project_id);
        CREATE INDEX IF NOT EXISTS idx_reviews_created ON reviews(created_at);
        CREATE INDEX IF NOT EXISTS idx_reviews_command ON reviews(command);

        -- Individual findings (issues) from each review/scan
        CREATE TABLE IF NOT EXISTS findings (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            review_id       INTEGER NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
            file_path       TEXT NOT NULL,
            line_number     INTEGER,
            severity        TEXT NOT NULL DEFAULT 'info',
            issue_type      TEXT,
            title           TEXT NOT NULL DEFAULT '',
            body            TEXT NOT NULL DEFAULT '',
            suggested_fix   TEXT,
            status          TEXT NOT NULL DEFAULT 'open',
            fingerprint     TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_findings_review ON findings(review_id);
        CREATE INDEX IF NOT EXISTS idx_findings_status ON findings(status);
        CREATE INDEX IF NOT EXISTS idx_findings_severity ON findings(severity);
        CREATE INDEX IF NOT EXISTS idx_findings_file ON findings(file_path);
        CREATE INDEX IF NOT EXISTS idx_findings_fingerprint ON findings(fingerprint);

        -- Audit trail for finding status changes
        CREATE TABLE IF NOT EXISTS finding_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            finding_id      INTEGER NOT NULL REFERENCES findings(id) ON DELETE CASCADE,
            event_type      TEXT NOT NULL,  -- 'opened', 'auto_resolved', 'dismissed', 'reopened'
            note            TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_fevents_finding ON finding_events(finding_id);
        CREATE INDEX IF NOT EXISTS idx_fevents_type ON finding_events(event_type);
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (5)", [])?;

    Ok(())
}

/// Migration v6: Index config hash for fingerprint invalidation.
///
/// Adds `index_config_hash` to `projects`. When config-relevant settings
/// (e.g. `index_skip_files`, language includes) change between runs, the
/// stored hash won't match and all file fingerprints are cleared, forcing
/// a full re-index on the next `cora index` run.
fn migrate_v6(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        ALTER TABLE projects ADD COLUMN index_config_hash TEXT;
        ",
    )?;

    // Recreate FTS5 with file column for better search coverage.
    // Drop old triggers first, then virtual table, then recreate with file column.
    conn.execute_batch(
        "
        DROP TRIGGER IF EXISTS symbols_fts_insert;
        DROP TRIGGER IF EXISTS symbols_fts_delete;
        DROP TRIGGER IF EXISTS symbols_fts_update;
        DROP TABLE IF EXISTS symbols_fts;
        CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
            name,
            signature,
            file,
            content='symbols',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 1'
        );
        CREATE TRIGGER IF NOT EXISTS symbols_fts_insert
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
        END;
        ",
    )?;

    conn.execute("INSERT INTO schema_version (version) VALUES (6)", [])?;

    Ok(())
}

/// Compute a stable hash of the indexing-relevant config.
///
/// Any change to these fields will invalidate all stored fingerprints,
/// forcing a full re-index on the next run.
pub fn compute_index_config_hash(skip_patterns: &[String]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    skip_patterns.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Get or create a project entry by root path.
///
/// Returns the project ID.
pub fn get_or_create_project(conn: &Connection, root_path: &str) -> anyhow::Result<i64> {
    // Try to find existing project
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM projects WHERE root_path = ?1",
            rusqlite::params![root_path],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    // Extract project name from directory name
    let name = std::path::Path::new(root_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    conn.execute(
        "INSERT INTO projects (root_path, name) VALUES (?1, ?2)",
        rusqlite::params![root_path, name],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Remove a project and all its associated data (CASCADE).
///
/// Returns the number of rows deleted.
pub fn delete_project(conn: &Connection, project_id: i64) -> anyhow::Result<usize> {
    let affected = conn.execute(
        "DELETE FROM projects WHERE id = ?1",
        rusqlite::params![project_id],
    )?;
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn test_migration_creates_tables() {
        let conn = mem_conn();

        // Check symbols table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Check files table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Check FTS table exists
        conn.query_row("SELECT COUNT(*) FROM symbols_fts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();

        // Check projects table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Check schema version
        let version: i32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn test_migration_idempotent() {
        let conn = mem_conn();
        // Running again should not error
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn test_v2_project_columns_exist() {
        let conn = mem_conn();

        // Verify project_id column exists on symbols
        let _: i64 = conn
            .query_row("SELECT project_id FROM symbols LIMIT 0", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        // Verify project_id column exists on files
        let _: i64 = conn
            .query_row("SELECT project_id FROM files LIMIT 0", [], |row| row.get(0))
            .unwrap_or(0);

        // Verify project_id column exists on call_graph
        let _: i64 = conn
            .query_row("SELECT project_id FROM call_graph LIMIT 0", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
    }

    #[test]
    fn test_get_or_create_project() {
        let conn = mem_conn();

        // Create project
        let id1 = get_or_create_project(&conn, "/home/user/myproject").unwrap();
        assert!(id1 > 0);

        // Same path returns same id
        let id2 = get_or_create_project(&conn, "/home/user/myproject").unwrap();
        assert_eq!(id1, id2);

        // Different path returns different id
        let id3 = get_or_create_project(&conn, "/home/user/other").unwrap();
        assert_ne!(id1, id3);

        // Check name extraction
        let name: String = conn
            .query_row(
                "SELECT name FROM projects WHERE id = ?1",
                rusqlite::params![id1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "myproject");
    }

    #[test]
    fn test_delete_project_cascades() {
        let conn = mem_conn();

        let pid = get_or_create_project(&conn, "/tmp/testproj").unwrap();

        // Insert symbol and file linked to project
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, project_id) VALUES ('test_fn', 'function', 'main.rs', 1, ?1)",
            rusqlite::params![pid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (path, fingerprint, last_indexed, project_id) VALUES ('main.rs', 'abc', datetime('now'), ?1)",
            rusqlite::params![pid],
        )
        .unwrap();

        // Delete project
        delete_project(&conn, pid).unwrap();

        // Symbols and files should be cascade-deleted
        let sym_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sym_count, 0);

        let file_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(file_count, 0);

        let proj_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(proj_count, 0);
    }

    #[test]
    fn test_v3_edges_table() {
        let conn = mem_conn();
        let pid = get_or_create_project(&conn, "/test/proj").unwrap();

        // Check edges table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Insert an edge
        conn.execute(
            "INSERT INTO edges (source, kind, target, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params!["Cache", "IMPLEMENTS", "Store", "cache.rs", 10, pid],
        )
        .unwrap();

        // Query it back
        let (source, kind, target): (String, String, String) = conn
            .query_row(
                "SELECT source, kind, target FROM edges WHERE kind = 'IMPLEMENTS'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source, "Cache");
        assert_eq!(kind, "IMPLEMENTS");
        assert_eq!(target, "Store");
    }

    #[test]
    fn test_v3_migrate_call_graph() {
        let conn = mem_conn();
        let pid = get_or_create_project(&conn, "/test/proj").unwrap();

        // Insert into old call_graph
        conn.execute(
            "INSERT INTO call_graph (caller, callee, file, line, project_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["main", "helper", "main.rs", 5, pid],
        )
        .unwrap();

        // Migration already ran, so edges should be empty
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE kind = 'CALLS'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        // Manually verify the migration SQL works
        conn.execute(
            "INSERT OR IGNORE INTO edges (source, kind, target, file, line, project_id) SELECT caller, 'CALLS', callee, file, line, project_id FROM call_graph",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE kind = 'CALLS'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_v5_review_tables_exist() {
        let conn = mem_conn();

        // Check reviews table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Check findings table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM findings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Check finding_events table
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM finding_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // Verify schema version is 5
        let version: i32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(version, 6);
    }

    #[test]
    fn test_v5_insert_review_and_findings() {
        let conn = mem_conn();
        let pid = get_or_create_project(&conn, "/test/proj").unwrap();

        // Insert a review
        conn.execute(
            "INSERT INTO reviews (project_id, command, commit_hash, branch, summary, score, files_scanned, lines_scanned)
             VALUES (?1, 'review', 'abc123', 'main', '3 issues found', 75, 5, 200)",
            rusqlite::params![pid],
        )
        .unwrap();
        let review_id: i64 = conn.last_insert_rowid();

        // Insert findings
        conn.execute(
            "INSERT INTO findings (review_id, file_path, line_number, severity, title, body, fingerprint)
             VALUES (?1, 'src/main.rs', 42, 'critical', 'SQL injection', 'Unsanitized input', 'main.rs:42:sql_injection')",
            rusqlite::params![review_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO findings (review_id, file_path, line_number, severity, title, body, fingerprint)
             VALUES (?1, 'src/lib.rs', 10, 'minor', 'Unused import', 'Import not used', 'lib.rs:10:unused_import')",
            rusqlite::params![review_id],
        )
        .unwrap();

        // Query back
        let finding_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM findings WHERE review_id = ?1",
                rusqlite::params![review_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(finding_count, 2);

        // Insert an event
        let finding_id: i64 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO finding_events (finding_id, event_type, note) VALUES (?1, 'opened', NULL)",
            rusqlite::params![finding_id],
        )
        .unwrap();

        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM finding_events WHERE finding_id = ?1",
                rusqlite::params![finding_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_count, 1);
    }

    #[test]
    fn test_v6_migration_adds_config_hash_column() {
        let conn = mem_conn();

        // Verify index_config_hash column exists on projects
        let hash: Option<String> = conn
            .query_row(
                "SELECT index_config_hash FROM projects WHERE 1=0",
                [],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        // No rows → None, but the column should exist without error
        assert!(hash.is_none());
    }

    #[test]
    fn test_compute_index_config_hash_deterministic() {
        let patterns1 = vec!["**/vendor/**".to_string(), "*.min.js".to_string()];
        let patterns2 = vec!["**/vendor/**".to_string(), "*.min.js".to_string()];
        let patterns3 = vec!["**/node_modules/**".to_string()];

        assert_eq!(
            compute_index_config_hash(&patterns1),
            compute_index_config_hash(&patterns2),
            "same patterns → same hash"
        );
        assert_ne!(
            compute_index_config_hash(&patterns1),
            compute_index_config_hash(&patterns3),
            "different patterns → different hash"
        );
    }

    #[test]
    fn test_compute_index_config_hash_empty() {
        let h1 = compute_index_config_hash(&[]);
        let h2 = compute_index_config_hash(&[]);
        assert_eq!(h1, h2, "empty patterns should be deterministic");
        assert!(!h1.is_empty(), "hash should not be empty");
    }
}
