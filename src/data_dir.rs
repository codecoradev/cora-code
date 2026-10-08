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
    let override_dir = std::env::var_os(CODECORA_HOME_ENV);
    // Unit tests must never read or write the developer's real `~/.codecora`
    // (the global vector index there is flock-ed by any running `cora`, #587).
    // Without an explicit override they get one process-wide scratch dir.
    #[cfg(test)]
    if override_dir.is_none() {
        return test_home().to_path_buf();
    }
    resolve_home(override_dir, dirs::home_dir())
}

/// Pure resolution rule behind [`codecora_home`] (no env/FS access).
fn resolve_home(override_dir: Option<std::ffi::OsString>, home: Option<PathBuf>) -> PathBuf {
    match override_dir {
        Some(dir) => PathBuf::from(dir),
        None => home
            .expect("Cannot determine home directory. Set CODECORA_HOME or HOME.")
            .join(".codecora"),
    }
}

/// Process-wide scratch data root for unit tests. One dir per test process
/// matches the process-global vector cache, and needs no env mutation, so
/// parallel tests cannot race on it. It is not removed at process exit; it
/// is small and lives under the OS temp dir.
#[cfg(test)]
fn test_home() -> &'static std::path::Path {
    static HOME: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        tempfile::Builder::new()
            .prefix("cora-test-home-")
            .tempdir()
            .expect("create test data dir")
            .keep()
    });
    &HOME
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

/// Path to the update check cache file.
///
/// Stored at `~/.codecora/cora-code/update-cache.json`.
pub fn update_cache_path() -> PathBuf {
    cora_data_dir().join("update-cache.json")
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
    fn resolve_home_uses_override_then_home() {
        assert_eq!(
            resolve_home(Some("/custom".into()), Some(PathBuf::from("/h"))),
            PathBuf::from("/custom")
        );
        assert_eq!(
            resolve_home(None, Some(PathBuf::from("/h"))),
            PathBuf::from("/h/.codecora")
        );
    }

    #[test]
    fn unit_tests_never_resolve_into_the_real_home() {
        if std::env::var_os(CODECORA_HOME_ENV).is_some() {
            return; // explicit override in the developer's shell: honoured
        }
        let real = dirs::home_dir().unwrap().join(".codecora");
        for p in [
            codecora_home(),
            cora_data_dir(),
            product_data_dir("x"),
            graph_db_path(),
        ] {
            assert!(!p.starts_with(&real), "{p:?} is under the real home");
            assert!(p.starts_with(test_home()), "{p:?} is not under test home");
        }
    }

    #[test]
    fn product_dirs_are_nested_under_home() {
        assert_eq!(
            product_data_dir("cora-code"),
            codecora_home().join("cora-code")
        );
        assert_eq!(cora_data_dir(), product_data_dir("cora-code"));
        let db = graph_db_path();
        assert!(
            db.ends_with("cora.db") || db.ends_with("graph.db"),
            "{db:?}"
        );
    }
}
