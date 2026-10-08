//! IndexBridge — the single seam for "which project am I in, and where is its index?".
//!
//! Every entry point (CLI arms, MCP tools, review-time scanners, the context
//! resolver) goes through this module. It owns three things:
//!
//! 1. **Root resolution** — a start path is always normalised with
//!    [`crate::index::resolve_project_root`], so a run from a subdirectory or a
//!    workspace member lands on the same `project_id` as indexing from the root.
//! 2. **Opening the database** — PRAGMAs and migrations live in
//!    [`crate::index::open_index_at`]; nobody else opens `cora.db` read-write.
//! 3. **Project id** — resolved once per bridge via `ensure_project`.
//!
//! Two modes:
//! - *tolerant* ([`IndexBridge::open`]): review-time. A missing database or any
//!   failure yields an *unavailable* bridge; all queries return empty results.
//! - *strict* ([`IndexBridge::open_strict`]): CLI/MCP. A missing database is a
//!   [`NoIndexError`]; the returned bridge is always available.
//!
//! [`IndexBridge::open_or_create`] is for writers (`index`, `serve`, `watch`).

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tracing::debug;

/// Returned by [`IndexBridge::open_strict`] when no index database exists yet.
#[derive(Debug)]
pub struct NoIndexError;

impl std::fmt::Display for NoIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "No index found. Run `cora index` first.")
    }
}

impl std::error::Error for NoIndexError {}

/// Bridge to the cora symbol index.
///
/// Holds an optional SQLite connection + project_id pair plus the resolved
/// project root. If the index is unavailable (tolerant mode: no `cora.db`,
/// migration failure, etc.) the bridge is *unavailable* but still safe to
/// query — all lookups return `None` / empty `Vec`.
pub struct IndexBridge {
    conn: Option<Connection>,
    project_id: Option<i64>,
    root: PathBuf,
}

impl IndexBridge {
    /// Normalise `start` to the project root (`.cora.yaml` / workspace / marker),
    /// falling back to `start` itself when no marker is found.
    pub fn resolve_root(start: &Path) -> PathBuf {
        crate::index::resolve_project_root(start).unwrap_or_else(|| start.to_path_buf())
    }

    /// Tolerant open of the global index for the project containing `start`.
    ///
    /// Never creates the database and never fails: use [`Self::is_available`].
    pub fn open(start: &Path) -> Self {
        Self::open_tolerant_at(&crate::data_dir::graph_db_path(), start)
    }

    /// Tolerant open of the global index for the current working directory.
    pub fn open_cwd() -> Self {
        match std::env::current_dir() {
            Ok(cwd) => Self::open(&cwd),
            Err(_) => Self::unavailable(),
        }
    }

    pub(crate) fn open_tolerant_at(db_path: &Path, start: &Path) -> Self {
        let root = Self::resolve_root(start);
        if !db_path.exists() {
            debug!("index bridge: no index database");
            return Self::unavailable_for(root);
        }
        match Self::open_at(db_path, &root) {
            Ok(b) => b,
            Err(e) => {
                debug!(error = %e, "index bridge: index unavailable");
                Self::unavailable_for(root)
            }
        }
    }

    /// Strict open of the global index: errors with [`NoIndexError`] if the
    /// database does not exist. The returned bridge is always available.
    pub fn open_strict(start: &Path) -> anyhow::Result<Self> {
        Self::open_strict_at(&crate::data_dir::graph_db_path(), start)
    }

    /// Strict open for the current working directory.
    pub fn open_strict_cwd() -> anyhow::Result<Self> {
        Self::open_strict(&std::env::current_dir()?)
    }

    pub(crate) fn open_strict_at(db_path: &Path, start: &Path) -> anyhow::Result<Self> {
        if !db_path.exists() {
            return Err(NoIndexError.into());
        }
        Self::open_at(db_path, &Self::resolve_root(start))
    }

    /// Open the global index, creating it if needed (writers: index/serve/watch).
    pub fn open_or_create(start: &Path) -> anyhow::Result<Self> {
        let conn = crate::index::open_global_index()?;
        Self::from_connection(conn, start)
    }

    /// [`Self::open_or_create`] for the current working directory.
    pub fn open_or_create_cwd() -> anyhow::Result<Self> {
        Self::open_or_create(&std::env::current_dir()?)
    }

    /// Wrap an existing connection (tests, in-memory indexes). Resolves the
    /// root from `start` and ensures the project row.
    pub fn from_connection(conn: Connection, start: &Path) -> anyhow::Result<Self> {
        let root = Self::resolve_root(start);
        let project_id = crate::index::ensure_project(&conn, &root)?;
        Ok(Self {
            conn: Some(conn),
            project_id: Some(project_id),
            root,
        })
    }

    fn open_at(db_path: &Path, root: &Path) -> anyhow::Result<Self> {
        let conn = crate::index::open_index_at(db_path)?;
        let project_id = crate::index::ensure_project(&conn, root)?;
        debug!(project_id, "index bridge: opened successfully");
        Ok(Self {
            conn: Some(conn),
            project_id: Some(project_id),
            root: root.to_path_buf(),
        })
    }

    /// Create an explicitly unavailable bridge (no index / cannot open).
    pub fn unavailable() -> Self {
        Self::unavailable_for(PathBuf::new())
    }

    fn unavailable_for(root: PathBuf) -> Self {
        Self {
            conn: None,
            project_id: None,
            root,
        }
    }

    /// Whether the bridge has an active index connection and a resolved project.
    #[inline]
    pub fn is_available(&self) -> bool {
        self.conn.is_some() && self.project_id.is_some()
    }

    /// The resolved project id (if available).
    #[allow(dead_code)]
    #[inline]
    pub fn project_id(&self) -> Option<i64> {
        self.project_id
    }

    /// The resolved project root (set even when the index is unavailable).
    #[inline]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Connection and project id together, or `None` when unavailable.
    #[inline]
    pub fn parts(&self) -> Option<(&Connection, i64)> {
        Some((self.conn.as_ref()?, self.project_id?))
    }

    /// Consume an available bridge into `(connection, project_id, root)`.
    pub fn into_strict_parts(self) -> anyhow::Result<(Connection, i64, PathBuf)> {
        let root = self.root;
        match (self.conn, self.project_id) {
            (Some(c), Some(id)) => Ok((c, id, root)),
            _ => Err(NoIndexError.into()),
        }
    }

    // ── Query helpers ────────────────────────────────────────────────────

    /// Search the symbols table via FTS5 for the given query text.
    ///
    /// Returns up to `limit` [`crate::index::symbols::SearchResult`] entries.
    /// Returns an empty vec if the bridge is unavailable.
    pub fn search_symbols(&self, query: &str, limit: usize) -> Vec<crate::index::SearchResult> {
        let conn = match self.conn.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let pid = match self.project_id {
            Some(id) => id,
            None => return Vec::new(),
        };

        let sq = crate::index::SymbolQuery::text(query);
        let mut sq = sq;
        sq.limit = limit;

        crate::index::search(conn, pid, &sq).unwrap_or_default()
    }

    /// Find callers of a symbol using the call-graph index.
    ///
    /// Returns up to `limit` [`crate::index::graph::CallerResult`] entries.
    /// Returns an empty vec if the bridge is unavailable.
    pub fn find_callers(
        &self,
        symbol_name: &str,
        limit: usize,
    ) -> Vec<crate::index::graph::CallerResult> {
        let conn = match self.conn.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let pid = match self.project_id {
            Some(id) => id,
            None => return Vec::new(),
        };

        crate::index::graph::find_callers(conn, pid, symbol_name, limit).unwrap_or_default()
    }

    /// Run a brain search (FTS5 + vector + graph RRF fusion).
    ///
    /// Returns up to `limit` [`crate::index::brain::BrainResult`] entries.
    /// Returns an empty vec if the bridge is unavailable.
    #[allow(dead_code)]
    pub fn brain_search(&self, query: &str, limit: usize) -> Vec<crate::index::brain::BrainResult> {
        let conn = match self.conn.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let pid = match self.project_id {
            Some(id) => id,
            None => return Vec::new(),
        };

        crate::index::brain::brain_search(conn, pid, query, limit).unwrap_or_default()
    }

    /// Run an impact analysis (blast radius) for the given symbol.
    ///
    /// Returns [`crate::index::graph::ImpactNode`] entries up to `depth`.
    /// Returns an empty vec if the bridge is unavailable.
    #[allow(dead_code)]
    pub fn impact_analysis(
        &self,
        symbol_name: &str,
        depth: u32,
    ) -> Vec<crate::index::graph::ImpactNode> {
        let conn = match self.conn.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let pid = match self.project_id {
            Some(id) => id,
            None => return Vec::new(),
        };

        crate::index::graph::impact_analysis(conn, pid, symbol_name, depth).unwrap_or_default()
    }

    /// Direct access to the underlying connection (if available).
    ///
    /// Used by callers that need raw SQL beyond the typed helpers above.
    /// Returns `None` if the bridge is unavailable.
    #[allow(dead_code)]
    #[inline]
    pub fn connection(&self) -> Option<&Connection> {
        self.conn.as_ref()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_bridge_reports_false() {
        let bridge = IndexBridge::unavailable();
        assert!(!bridge.is_available());
        assert_eq!(bridge.project_id(), None);
        assert!(bridge.search_symbols("foo", 10).is_empty());
        assert!(bridge.find_callers("foo", 10).is_empty());
        assert!(bridge.brain_search("foo", 10).is_empty());
        assert!(bridge.connection().is_none());
    }

    fn init_db(dir: &Path) -> PathBuf {
        let db = dir.join("cora.db");
        crate::index::open_index_at(&db).unwrap();
        db
    }

    #[test]
    fn tolerant_open_without_index_is_unavailable_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cora.db");
        let bridge = IndexBridge::open_tolerant_at(&db, dir.path());
        assert!(!bridge.is_available());
        assert!(bridge.parts().is_none());
        assert!(!db.exists(), "tolerant mode must not create the database");
    }

    #[test]
    fn strict_open_without_index_errors_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cora.db");
        let err = IndexBridge::open_strict_at(&db, dir.path())
            .err()
            .expect("strict mode must fail without an index");
        assert!(err.downcast_ref::<NoIndexError>().is_some());
        assert!(err.to_string().contains("cora index"));
    }

    #[test]
    fn subdirectory_resolves_same_project_as_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let member = repo.join("crates/member/src");
        std::fs::create_dir_all(&member).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/member\"]\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("crates/member/Cargo.toml"),
            "[package]\nname = \"member\"\n",
        )
        .unwrap();
        let db = init_db(dir.path());

        let from_root = IndexBridge::open_strict_at(&db, &repo).unwrap();
        let from_sub = IndexBridge::open_tolerant_at(&db, &member);
        assert!(from_sub.is_available());
        assert_eq!(from_root.project_id(), from_sub.project_id());
        assert_eq!(from_root.root(), from_sub.root());
    }

    #[test]
    fn from_connection_resolves_root_from_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join(".cora.yaml"), "").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::ensure_project(&conn, dir.path()).unwrap();
        let bridge = IndexBridge::from_connection(conn, &sub).unwrap();
        assert_eq!(bridge.project_id(), Some(pid));
    }

    #[test]
    fn pragmas_are_applied_by_the_shared_opener() {
        let dir = tempfile::tempdir().unwrap();
        let db = init_db(dir.path());
        let conn = crate::index::open_index_at(&db).unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!((fk, sync, mode.as_str()), (1, 1, "wal"));
    }
}
