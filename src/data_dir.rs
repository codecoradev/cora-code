//! CodeCora data directory resolution.
//!
//! All CodeCora products store runtime data under `$HOME/.codecora/{product}/`.
//! This module provides the shared resolution logic.

use std::path::PathBuf;

/// Environment variable to override the CodeCora data root.
/// When set, all products use this as the parent directory instead of `$HOME/.codecora/`.
pub const CODECORA_HOME_ENV: &str = "CODECORA_HOME";

/// Returns the CodeCora data directory: `$HOME/.codecora/` (or `CODECORA_HOME` override).
///
/// ```text
/// CODECORA_HOME=/custom  → /custom
/// (not set)              → $HOME/.codecora/
/// ```
pub fn codecora_home() -> PathBuf {
    if let Ok(home) = std::env::var(CODECORA_HOME_ENV) {
        PathBuf::from(home)
    } else {
        dirs::home_dir()
            .expect("Cannot determine home directory. Set CODECORA_HOME or HOME.")
            .join(".codecora")
    }
}

/// Returns the data directory for a specific CodeCora product.
///
/// ```text
/// product_data_dir("cora-code") → $HOME/.codecora/cora-code/
/// ```
pub fn product_data_dir(product: &str) -> PathBuf {
    codecora_home().join(product)
}

/// Returns the cora-code data directory: `$HOME/.codecora/cora-code/`.
pub fn cora_data_dir() -> PathBuf {
    product_data_dir("cora-code")
}

/// Returns the path to the global graph database.
///
/// Defaults to `cora.db`. If `cora.db` does not exist but `graph.db` does,
/// automatically renames `graph.db` → `cora.db` (backward-compatible migration).
pub fn graph_db_path() -> PathBuf {
    let dir = cora_data_dir();
    let new_db = dir.join("cora.db");

    if !new_db.exists() {
        let old_db = dir.join("graph.db");
        if old_db.exists() {
            if let Err(e) = std::fs::rename(&old_db, &new_db) {
                eprintln!("warning: failed to migrate graph.db → cora.db: {e}");
                // Fall back to graph.db on rename failure
                return old_db;
            }
        }
    }

    new_db
}

/// Ensure the cora-code data directory exists.
pub fn ensure_data_dir() -> anyhow::Result<PathBuf> {
    let dir = cora_data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codecora_home_returns_path() {
        let path = codecora_home();
        assert!(path.ends_with(".codecora"));
    }

    #[test]
    fn test_product_data_dir() {
        let path = product_data_dir("cora-code");
        assert!(path.ends_with(".codecora/cora-code"));
    }

    #[test]
    fn test_graph_db_path() {
        let path = graph_db_path();
        assert!(
            path.ends_with(".codecora/cora-code/cora.db") || path.ends_with(".codecora/cora-code/graph.db"),
            "graph_db_path should end with cora.db (or graph.db on migration failure), got: {path:?}"
        );
    }

    #[test]
    fn test_cora_data_dir() {
        let path = cora_data_dir();
        assert!(path.ends_with(".codecora/cora-code"));
        // Should not have trailing slash
        let s = path.to_string_lossy();
        assert!(!s.ends_with('/'));
    }

    #[test]
    fn test_env_override() {
        unsafe {
            std::env::set_var(CODECORA_HOME_ENV, "/tmp/test-codecora");
        }
        let path = codecora_home();
        assert_eq!(path, PathBuf::from("/tmp/test-codecora"));
        unsafe {
            std::env::remove_var(CODECORA_HOME_ENV);
        }
    }
}
