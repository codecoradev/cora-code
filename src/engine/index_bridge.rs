//! IndexBridge — lightweight connection between the engine and the symbol index.
//!
//! Provides a single struct that wraps an optional `rusqlite::Connection` to the
//! global `cora.db` and the resolved `project_id`.  When the index database does
//! not exist or cannot be opened, the bridge reports `is_available() == false`
//! and all query methods return empty results — **zero caller impact**.
//!
//! The bridge is constructed once at the start of a review/scan run and passed
//! through the context chain pipeline, replacing the ad-hoc
//! `crate::index::open_global_index()` calls scattered throughout resolver.rs.

use std::path::Path;

use rusqlite::Connection;
use tracing::debug;

// ── Public API ────────────────────────────────────────────────────────

/// Bridge to the cora symbol index.
///
/// Holds an optional SQLite connection + project_id pair.  If the index is
/// unavailable (no `cora.db`, migration failure, etc.) the bridge is *unavailable*
/// but still safe to query — all lookups return `None` / empty `Vec`.
#[allow(dead_code)]
pub struct IndexBridge {
    conn: Option<Connection>,
    project_id: Option<i64>,
}

impl IndexBridge {
    /// Open the global index and resolve the project id for `project_root`.
    ///
    /// Returns an `IndexBridge` regardless of whether the index exists.
    /// Call `is_available()` to check.
    pub fn open(project_root: &Path) -> Self {
        let conn = match crate::index::open_global_index() {
            Ok(c) => c,
            Err(e) => {
                debug!(error = %e, "index bridge: global index unavailable");
                return Self::unavailable();
            }
        };

        let project_id = match crate::index::ensure_project(&conn, project_root) {
            Ok(id) => Some(id),
            Err(e) => {
                debug!(error = %e, "index bridge: failed to resolve project_id");
                return Self::unavailable();
            }
        };

        debug!(project_id, "index bridge: opened successfully");
        Self {
            conn: Some(conn),
            project_id,
        }
    }

    /// Create an explicitly unavailable bridge (no index / cannot open).
    pub fn unavailable() -> Self {
        Self {
            conn: None,
            project_id: None,
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

    // ── Query helpers ────────────────────────────────────────────────────

    /// Search the symbols table via FTS5 for the given query text.
    ///
    /// Returns up to `limit` [`crate::index::symbols::SearchResult`] entries.
    /// Returns an empty vec if the bridge is unavailable.
    pub fn search_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<crate::index::SearchResult> {
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
    pub fn brain_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<crate::index::brain::BrainResult> {
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

    #[test]
    fn open_nonexistent_project_returns_unavailable() {
        // Opening with a nonexistent project root should still succeed
        // (it creates the project row), but we can verify it opens.
        let dir = tempfile::tempdir().unwrap();
        // The bridge opens the global index — if it doesn't exist,
        // the data_dir crate will create it.
        let bridge = IndexBridge::open(dir.path());
        // Either available (index was created) or unavailable — both are valid.
        // The key invariant: no panic, no crash.
        let _ = bridge.is_available();
    }
}
