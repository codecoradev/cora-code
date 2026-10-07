//! Index session — the one place that owns
//! config → embedding backend → vector store → skip patterns → project root →
//! incremental index → stats.
//!
//! Every entry point that writes or searches the index (`cora index`,
//! `index --watch`, `cora watch`, `cora serve`, `cora brain`, the MCP brain
//! tool) goes through this module instead of re-assembling the same steps by
//! hand. The project root and database come from
//! [`IndexBridge`](crate::engine::index_bridge::IndexBridge).
//!
//! # Config policy
//!
//! [`ConfigSource`] chooses how config is loaded:
//! - [`ConfigSource::Full`]: the normal CLI stack (global config, explicit
//!   `--config` path or discovered `.cora.yaml`).
//! - [`ConfigSource::ProjectOnly`]: MCP stance (#563). Only the project's
//!   `.cora.yaml`, merged over defaults. No env, no global config, no API keys.
//!
//! Both feed the *same* [`configure`] step, so the embedding backend
//! (`brain.embedding`), vector store/bits and skip patterns are applied
//! identically. `resolve_backend` only consumes the `brain.embedding` string
//! (a local model choice, not a secret and not a network endpoint), so running
//! it with project-only config does not widen the MCP trust surface.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

use super::{IndexStats, IndexSummary};
use crate::config::schema::Config;
use crate::embed::Backend;
use crate::engine::index_bridge::IndexBridge;

/// How an entry point loads config.
#[derive(Debug, Clone, Copy)]
pub enum ConfigSource<'a> {
    /// Global config + explicit `--config` path (or discovered `.cora.yaml`).
    Full(Option<&'a str>),
    /// Project `.cora.yaml` only (MCP: no env/global/secrets).
    ProjectOnly,
    /// [`Self::ProjectOnly`] discovered from `start` instead of the cwd.
    #[cfg_attr(not(test), allow(dead_code))]
    ProjectOnlyAt(&'a Path),
}

/// Load config per `source`. `None` when it cannot be loaded (callers fall
/// back to defaults, as every entry point did before).
pub fn load_config(source: ConfigSource<'_>) -> Option<Config> {
    match source {
        ConfigSource::Full(path) => {
            crate::config::loader::load_config(path, None, None, None, None, false).ok()
        }
        ConfigSource::ProjectOnly => std::env::current_dir()
            .ok()
            .and_then(|cwd| load_project_only(&cwd)),
        ConfigSource::ProjectOnlyAt(start) => load_project_only(start),
    }
}

/// Project-only config found by walking up from `start` (no env/global).
pub fn load_project_only(start: &Path) -> Option<Config> {
    let mut config = Config::default();
    if let Some((_, cora)) = crate::config::loader::find_cora_file(start).ok()? {
        cora.merge_into(&mut config).ok()?;
    }
    Some(config)
}

/// Result of [`configure`]: what was applied to the process.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))] // brain_mode/backend are the test seam
pub struct Configured {
    /// `brain.embedding` string that was handed to `resolve_backend`.
    pub brain_mode: String,
    /// Backend now active (process-wide; the first resolution wins).
    pub backend: Backend,
    /// Index exclusion patterns (`None` when no config could be loaded).
    pub skip_patterns: Option<Vec<String>>,
}

/// Apply config to process-global state (embedding backend, vector store) and
/// derive the skip patterns. The only caller of `resolve_backend` for index
/// and brain code paths.
pub fn configure(config: Option<&Config>) -> Configured {
    let brain_mode = config
        .map(|c| c.brain.embedding.to_string())
        .unwrap_or_else(|| "auto".to_string());
    let backend = crate::embed::resolve_backend(&brain_mode);
    super::vector::apply_config_store(config);
    Configured {
        brain_mode,
        backend,
        skip_patterns: super::skip_patterns_from_config(config),
    }
}

/// Load + [`configure`] for read-only brain search (CLI `brain`, MCP brain).
pub fn configure_for_search(source: ConfigSource<'_>) -> Configured {
    configure(load_config(source).as_ref())
}

/// An open, configured index for one project.
pub struct IndexSession {
    conn: Connection,
    project_id: i64,
    root: PathBuf,
    skip_patterns: Option<Vec<String>>,
}

impl IndexSession {
    /// Open (creating if needed) the global index for the project containing
    /// the current directory and configure the process.
    pub fn open(source: ConfigSource<'_>) -> Result<Self> {
        Self::open_at(&std::env::current_dir()?, source)
    }

    /// [`Self::open`] for the project containing `start`.
    pub fn open_at(start: &Path, source: ConfigSource<'_>) -> Result<Self> {
        let bridge = IndexBridge::open_or_create(start)?;
        Self::from_bridge(bridge, load_config(source).as_ref())
    }

    /// Build a session from an already-open bridge and loaded config.
    pub fn from_bridge(bridge: IndexBridge, config: Option<&Config>) -> Result<Self> {
        let (conn, project_id, root) = bridge.into_strict_parts()?;
        let configured = configure(config);
        Ok(Self {
            conn,
            project_id,
            root,
            skip_patterns: configured.skip_patterns,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn skip_patterns(&self) -> Option<&[String]> {
        self.skip_patterns.as_deref()
    }

    /// Incremental index with the session's skip patterns.
    pub fn index(&self, verbose: bool) -> Result<IndexStats> {
        super::index_project_with_skip(&self.conn, &self.root, verbose, self.skip_patterns())
    }

    /// Stored totals for this project.
    pub fn summary(&self) -> Result<IndexSummary> {
        super::index_stats(&self.conn, self.project_id)
    }

    /// Remove index rows for files that no longer exist. Returns the count.
    pub fn prune(&self) -> Result<usize> {
        super::prune_deleted(&self.conn, self.project_id, &self.root)
    }

    /// Drop everything stored for this project and re-register it.
    pub fn rebuild(&mut self) -> Result<()> {
        super::schema::delete_project(&self.conn, self.project_id)?;
        self.project_id =
            super::schema::get_or_create_project(&self.conn, &self.root.to_string_lossy())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(cora_yaml: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::write(root.join(".cora.yaml"), cora_yaml).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").unwrap();
        fs::create_dir_all(root.join("gen")).unwrap();
        fs::write(root.join("gen/b.rs"), "pub fn beta() {}\n").unwrap();
        fs::create_dir_all(root.join("vendor")).unwrap();
        fs::write(root.join("vendor/c.rs"), "pub fn gamma() {}\n").unwrap();
        (dir, root)
    }

    const YAML: &str = "brain:\n  embedding: hashing\nignore:\n  files:\n    - \"gen/**\"\nrules_engine:\n  index_skip_files:\n    - \"vendor/**\"\n";

    fn memory_bridge(root: &Path) -> IndexBridge {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        IndexBridge::from_connection(conn, root).unwrap()
    }

    #[test]
    fn project_only_config_merges_skip_sources_and_resolves_backend() {
        let (_d, root) = project(YAML);
        let config = load_project_only(&root).expect("config");
        let configured = configure(Some(&config));
        assert_eq!(configured.brain_mode, "hashing");
        assert_eq!(configured.backend, Backend::Hashing);
        let pats = configured.skip_patterns.unwrap();
        assert!(pats.contains(&"gen/**".to_string()), "{pats:?}");
        assert!(pats.contains(&"vendor/**".to_string()), "{pats:?}");
    }

    #[test]
    fn search_setup_resolves_backend_from_project_config() {
        // The MCP brain tool calls configure_for_search(ProjectOnly): the
        // backend string must come from the project's .cora.yaml.
        let (_d, root) = project(YAML);
        let configured = configure_for_search(ConfigSource::ProjectOnlyAt(&root));
        assert_eq!(configured.brain_mode, "hashing");
        assert_eq!(configured.backend, Backend::Hashing);
    }

    #[test]
    fn explicit_config_path_is_honored_by_full_source() {
        // `cora --config X serve` threads X through ConfigSource::Full.
        let (_d, root) = project("brain:\n  embedding: hashing\n");
        let other = root.join("other.yaml");
        fs::write(&other, "ignore:\n  files:\n    - \"only-in-explicit/**\"\n").unwrap();
        let config = load_config(ConfigSource::Full(other.to_str())).expect("config");
        let pats = crate::index::skip_patterns_from_config(Some(&config)).unwrap();
        assert!(
            pats.contains(&"only-in-explicit/**".to_string()),
            "{pats:?}"
        );
    }

    #[test]
    fn session_indexes_incrementally_and_excludes_skipped() {
        let (_d, root) = project(YAML);
        let config = load_project_only(&root);
        let session = IndexSession::from_bridge(memory_bridge(&root), config.as_ref()).unwrap();
        assert_eq!(session.root(), root.as_path());

        let first = session.index(false).unwrap();
        assert_eq!(first.files_indexed, 1, "only src/a.rs: {first:?}");
        assert!(first.files_excluded >= 2, "{first:?}");
        assert_eq!(session.summary().unwrap().total_files, 1);

        // Second run: nothing changed.
        let second = session.index(false).unwrap();
        assert_eq!(second.files_indexed, 0, "{second:?}");
        assert!(second.files_skipped >= 1, "{second:?}");

        // Change a file: only it is re-indexed.
        fs::write(
            root.join("src/a.rs"),
            "pub fn alpha() {}\npub fn alpha2() {}\n",
        )
        .unwrap();
        let third = session.index(false).unwrap();
        assert_eq!(third.files_indexed, 1, "{third:?}");
    }

    #[test]
    fn rebuild_resets_project_and_prune_counts_deleted() {
        let (_d, root) = project(YAML);
        let config = load_project_only(&root);
        let mut session = IndexSession::from_bridge(memory_bridge(&root), config.as_ref()).unwrap();
        session.index(false).unwrap();
        fs::remove_file(root.join("src/a.rs")).unwrap();
        assert_eq!(session.prune().unwrap(), 1);

        session.index(false).unwrap();
        session.rebuild().unwrap();
        assert_eq!(session.summary().unwrap().total_files, 0);
    }

    #[test]
    fn scanners_and_indexing_share_one_skip_source() {
        let (_d, root) = project(YAML);
        let config = load_project_only(&root).unwrap();
        let session = IndexSession::from_bridge(memory_bridge(&root), Some(&config)).unwrap();
        let review_patterns = crate::engine::deterministic::skip_patterns(&config);
        assert_eq!(Some(review_patterns.as_slice()), session.skip_patterns());
        let matcher = crate::engine::path_match::PathMatcher::new(&review_patterns);
        assert!(matcher.is_match("gen/b.rs"));
        assert!(matcher.is_match("vendor/c.rs"));
    }
}
