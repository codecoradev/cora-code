//! Persistent vector index using usearch (HNSW) for symbol embeddings.
//!
//! Pattern copied from uteke-core/src/memory/vector.rs, simplified for cora-code:
//! - Keys are symbol database IDs (i64) instead of UUID strings
//! - Single purpose: code symbol semantic search

// Public API reserved for Phase 4+ (remove, dims, etc.).
#![allow(dead_code)]

use anyhow::{Context, Result};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const USEARCH_EXT: &str = "usearch";
const VECQ_EXT: &str = "vecq";
const VECQ_SEED: u64 = 42;

/// Which physical vector store backs `CodeVectorIndex` (#542).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorStoreKind {
    /// HNSW graph over f32 vectors (the historical default).
    #[default]
    Usearch,
    /// vecq 4-bit quantized scan — pure Rust, deterministic, ~6x smaller.
    Vecq,
}

impl VectorStoreKind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "vecq" => Self::Vecq,
            _ => Self::Usearch, // "usearch", "" and anything unknown
        }
    }
}

static VECTOR_STORE: LazyLock<std::sync::RwLock<VectorStoreKind>> =
    LazyLock::new(|| std::sync::RwLock::new(VectorStoreKind::Usearch));

/// Select the physical vector store used by NEW indexes (process-wide).
/// Existing on-disk indexes keep their own format via file extension.
pub fn set_vector_store(kind: VectorStoreKind) {
    *VECTOR_STORE.write().unwrap() = kind;
}

pub fn current_vector_store() -> VectorStoreKind {
    *VECTOR_STORE.read().unwrap()
}

/// Apply `brain.vector_store` from a loaded config (any process entry that
/// touches Brain search/embedding must call this before opening the index,
/// or `.vecq`/`.usearch` files silently diverge between runs).
pub fn apply_config_store(config: Option<&crate::config::schema::Config>) {
    let kind = config
        .map(|c| VectorStoreKind::parse(&c.brain.vector_store))
        .unwrap_or_default();
    set_vector_store(kind);
}

/// Hashing-trick embedding dimensions (zero-dependency fallback).
pub const FALLBACK_DIMS: usize = 256;

/// Default embedding dimensions for the vector index.
///
/// When compiled with `pretrained-embed`, uses nomic-embed-code 768-dim vectors.
/// Otherwise falls back to the hashing-trick 256-dim vectors.
#[cfg(feature = "pretrained-embed")]
pub const DEFAULT_DIMS: usize = 768;

#[cfg(not(feature = "pretrained-embed"))]
pub const DEFAULT_DIMS: usize = FALLBACK_DIMS;

/// Persistent HNSW vector index for code symbol embeddings.
///
/// - Keys are symbol database IDs (i64)
/// - Cosine distance metric
/// - Disk persistence via buffer-based serialization (same as uteke)
/// - Cross-process safety via exclusive file lock
pub struct CodeVectorIndex {
    inner: Inner,
    path: Option<PathBuf>,
    dirty: bool,
    _lock_file: Option<File>,
}

enum Inner {
    Usearch {
        index: Index,
        /// Maps usearch integer key → symbol database ID.
        key_to_symbol: HashMap<u64, i64>,
        /// Maps symbol database ID → usearch integer key.
        symbol_to_key: HashMap<i64, u64>,
        next_key: u64,
    },
    /// vecq keys are u64 natively (tombstone removal + compact), so symbol
    /// IDs map directly — no sidecar needed.
    Vecq(Box<vecq_core::VecqIndex>),
}

impl CodeVectorIndex {
    /// Create a new empty in-memory index (backend selected by
    /// [`current_vector_store`]).
    pub fn new(dims: usize) -> Result<Self> {
        Ok(Self {
            inner: match current_vector_store() {
                VectorStoreKind::Usearch => Inner::Usearch {
                    index: create_usearch_index(dims)?,
                    key_to_symbol: HashMap::new(),
                    symbol_to_key: HashMap::new(),
                    next_key: 0,
                },
                VectorStoreKind::Vecq => {
                    Inner::Vecq(Box::new(vecq_core::VecqIndex::new(dims, VECQ_SEED)))
                }
            },
            path: None,
            dirty: false,
            _lock_file: None,
        })
    }

    /// Load from disk, or create empty if not exists.
    /// Acquires exclusive file lock for cross-process safety.
    ///
    /// Each backend owns its own file extension (`.usearch` vs `.vecq`), so
    /// switching `brain.vector_store` never misreads the other's format.
    pub fn load_or_create(path: &Path, dims: usize) -> Result<Self> {
        match current_vector_store() {
            VectorStoreKind::Vecq => {
                Self::load_or_create_vecq(&path.with_extension(VECQ_EXT), dims)
            }
            VectorStoreKind::Usearch => Self::load_or_create_usearch(path, dims),
        }
    }

    fn load_or_create_usearch(path: &Path, dims: usize) -> Result<Self> {
        if !path.exists() {
            std::fs::write(path, []).context("create usearch file")?;
        }

        let mut lock_file = acquire_file_lock(path)?;

        let mut idx = if lock_file.metadata().context("read file metadata")?.len() == 0 {
            Self::new(dims)?
        } else {
            Self::load_from_file(&mut lock_file, path)?
        };
        idx.path = Some(path.to_path_buf());
        idx._lock_file = Some(lock_file);
        Ok(idx)
    }

    fn load_or_create_vecq(path: &Path, dims: usize) -> Result<Self> {
        if !path.exists() {
            std::fs::write(path, []).context("create vecq file")?;
        }
        let mut lock_file = acquire_file_lock(path)?;

        let mut buffer = Vec::new();
        if lock_file.metadata().context("read vecq metadata")?.len() > 0 {
            use std::io::{Read, Seek, SeekFrom};
            lock_file
                .seek(SeekFrom::Start(0))
                .context("seek vecq file")?;
            lock_file
                .read_to_end(&mut buffer)
                .context("read vecq file")?;
        }

        // KNOWN LIMITATION (#542): vecq-core 0.2.0 `from_bytes` restores codes
        // and scales but NOT the keyed map (see codecoradev/vecq#32), so a
        // reloaded index would silently search empty. Until key serialization
        // ships upstream, reload always rebuilds fresh — safe (never
        // mis-searches); `cora index` re-embeds on the next run.
        if buffer.len() >= 24 {
            tracing::warn!(
                "vecq persistence lacks key serialization upstream (vecq#32) —                  recreating index; run `cora index` to re-embed symbols"
            );
        }
        let index = vecq_core::VecqIndex::new(dims, VECQ_SEED);

        Ok(Self {
            inner: Inner::Vecq(Box::new(index)),
            path: Some(path.to_path_buf()),
            dirty: false,
            _lock_file: Some(lock_file),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn load_from_file(file: &mut File, path: &Path) -> Result<Self> {
        use std::io::{Read, Seek, SeekFrom};

        file.seek(SeekFrom::Start(0)).context("seek usearch file")?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).context("read usearch file")?;

        let index =
            Index::restore_from_buffer(&buffer).context("load usearch index from buffer")?;

        // Rebuild key mappings from sidecar
        let mut key_to_symbol = HashMap::new();
        let mut symbol_to_key = HashMap::new();
        let mut next_key = 0u64;

        let mapping_path = path.with_extension("keys");
        if mapping_path.exists() {
            let data = std::fs::read_to_string(&mapping_path).context("read key mapping file")?;
            for line in data.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some((key_str, sym_id)) = line.split_once('\t') {
                    if let (Ok(key), Ok(sym)) = (key_str.parse::<u64>(), sym_id.parse::<i64>()) {
                        key_to_symbol.insert(key, sym);
                        symbol_to_key.insert(sym, key);
                        next_key = next_key.max(key + 1);
                    }
                }
            }
        }

        Ok(Self {
            inner: Inner::Usearch {
                index,
                key_to_symbol,
                symbol_to_key,
                next_key,
            },
            path: None,
            dirty: false,
            _lock_file: None,
        })
    }

    /// Save index and key mappings to disk.
    pub fn save(&mut self) -> Result<()> {
        if let Inner::Vecq(index) = &mut self.inner {
            let path = self
                .path
                .as_ref()
                .context("vecq index has no path")?
                .with_extension(VECQ_EXT);
            let buffer = index.to_bytes();
            let tmp_path = path.with_extension(format!("{VECQ_EXT}.tmp"));
            std::fs::write(&tmp_path, &buffer).context("write temp vecq index")?;
            std::fs::rename(&tmp_path, path).context("rename temp vecq")?;
            self.dirty = false;
            return Ok(());
        }
        if let Some(ref path) = self.path {
            let Inner::Usearch {
                index,
                key_to_symbol,
                ..
            } = &self.inner
            else {
                unreachable!("vecq path returned earlier");
            };
            let buf_len = index.serialized_length();
            let mut buffer = vec![0u8; buf_len];
            index
                .save_to_buffer(&mut buffer)
                .context("save usearch to buffer")?;

            let tmp_path = path.with_extension(format!("{USEARCH_EXT}.tmp"));
            std::fs::write(&tmp_path, &buffer).context("write temp usearch index")?;
            std::fs::rename(&tmp_path, path).context("rename temp usearch")?;

            // Save key mapping sidecar
            let mapping_path = path.with_extension("keys");
            let mut lines = Vec::new();
            for (&key, &sym_id) in key_to_symbol {
                lines.push(format!("{key}\t{sym_id}"));
            }
            atomic_write(&mapping_path, lines.join("\n").as_bytes())?;

            self.dirty = false;
        }
        Ok(())
    }

    /// Insert a symbol embedding. If symbol ID exists, replaces it.
    pub fn insert(&mut self, symbol_id: i64, embedding: &[f32]) -> Result<()> {
        if let Inner::Vecq(index) = &mut self.inner {
            let key = symbol_id as u64;
            if index.contains_key(key) {
                index.remove_keyed(key);
            }
            index.add_keyed(key, embedding);
            self.dirty = true;
            return Ok(());
        }
        let Inner::Usearch {
            index,
            key_to_symbol,
            symbol_to_key,
            next_key,
        } = &mut self.inner
        else {
            unreachable!("usearch insert path guarded above");
        };

        // Remove old entry if exists
        if let Some(&old_key) = symbol_to_key.get(&symbol_id) {
            key_to_symbol.remove(&old_key);
            index.remove(old_key).context("remove old usearch entry")?;
        }

        let key = *next_key;
        *next_key += 1;
        key_to_symbol.insert(key, symbol_id);
        symbol_to_key.insert(symbol_id, key);

        // Auto-reserve if at capacity
        if index.size() >= index.capacity() {
            let new_cap = (index.capacity() + 1024).max(1024);
            index.reserve(new_cap).context("reserve usearch capacity")?;
        }

        index.add(key, embedding).context("insert into usearch")?;

        self.dirty = true;
        Ok(())
    }

    /// Remove a symbol by database ID. Incremental, no rebuild.
    pub fn remove(&mut self, symbol_id: i64) -> bool {
        if let Inner::Vecq(index) = &mut self.inner {
            let removed = index.remove_keyed(symbol_id as u64);
            self.dirty |= removed;
            return removed;
        }
        let Inner::Usearch {
            index,
            key_to_symbol,
            symbol_to_key,
            next_key: _,
        } = &mut self.inner
        else {
            return false;
        };
        if let Some(&key) = symbol_to_key.get(&symbol_id) {
            key_to_symbol.remove(&key);
            symbol_to_key.remove(&symbol_id);
            if let Err(e) = index.remove(key) {
                tracing::error!("Failed to remove from usearch: {e}");
            }
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Search for k nearest neighbors. Returns (symbol_id, cosine_distance) pairs.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(i64, f32)> {
        if let Inner::Vecq(index) = &self.inner {
            if index.is_empty() {
                return Vec::new();
            }
            let count = k.max(1);
            return index
                .search_keyed(query, count)
                .into_iter()
                .map(|(key, sim)| (key as i64, 1.0 - sim))
                .collect();
        }
        if self.index_size() == 0 {
            return Vec::new();
        }
        let count = k.max(1);
        let Inner::Usearch {
            index,
            key_to_symbol,
            ..
        } = &self.inner
        else {
            unreachable!("vecq path returned earlier");
        };
        let results = match index.search(query, count) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("usearch search failed: {e}");
                return Vec::new();
            }
        };

        results
            .keys
            .iter()
            .zip(results.distances.iter())
            .filter_map(|(key, dist)| key_to_symbol.get(key).map(|&id| (id, *dist)))
            .collect()
    }

    /// Number of vectors in the index.
    pub fn len(&self) -> usize {
        match &self.inner {
            Inner::Usearch { index, .. } => index.size(),
            Inner::Vecq(index) => index.len(),
        }
    }

    /// Embedding dimensionality.
    pub fn dims(&self) -> usize {
        match &self.inner {
            Inner::Usearch { index, .. } => index.dimensions(),
            Inner::Vecq(index) => index.dim(),
        }
    }

    /// Whether the index has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[allow(dead_code)]
    fn index_size(&self) -> usize {
        self.len()
    }
}

fn create_usearch_index(dims: usize) -> Result<Index> {
    let options = IndexOptions {
        dimensions: dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        ..Default::default()
    };
    Index::new(&options).context("create usearch index")
}

/// Convert cosine distance (0..2) to cosine similarity (0..1).
pub fn cosine_distance_to_similarity(distance: f32) -> f32 {
    (1.0 - distance).clamp(0.0, 1.0)
}

fn acquire_file_lock(path: &Path) -> Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open usearch file for locking: {}", path.display()))?;

    if file.try_lock_exclusive().is_ok() {
        tracing::debug!("usearch file lock acquired: {}", path.display());
    } else {
        tracing::debug!("usearch file lock busy, waiting...");
        file.lock_exclusive()
            .context("acquire exclusive file lock on usearch")?;
    }
    Ok(file)
}

fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<()> {
    let tmp_path = path.with_extension("keys.tmp");
    std::fs::write(&tmp_path, data).context("write temp key mapping")?;
    std::fs::rename(&tmp_path, path).context("rename temp to final key mapping")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32;

    fn make_unit_vec(dims: usize, idx: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        if idx < dims {
            v[idx] = 1.0;
        }
        v
    }

    /// The global store selector is process-wide — serialize every test that
    /// constructs an index so backend flips never race parallel tests.
    static STORE_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    fn with_store_lock() -> std::sync::MutexGuard<'static, ()> {
        STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_empty_search() {
        let _g = with_store_lock();
        let idx = CodeVectorIndex::new(768).unwrap();
        assert!(idx.is_empty());
        let results = idx.search(&[0.0f32; 768], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_insert_and_search() {
        let _g = with_store_lock();
        let mut idx = CodeVectorIndex::new(768).unwrap();

        let v1 = make_unit_vec(768, 0); // unit vector along dim 0
        let v2 = make_unit_vec(768, 1); // unit vector along dim 1
        let mut v3 = vec![0.0f32; 768];
        v3[0] = 0.9;
        v3[1] = 0.1;
        let norm = v3.iter().map(|x| x * x).sum::<f32>().sqrt();
        v3.iter_mut().for_each(|x| *x /= norm);

        idx.insert(100, &v1).unwrap();
        idx.insert(200, &v2).unwrap();
        idx.insert(300, &v3).unwrap();
        assert_eq!(idx.len(), 3);

        // Query with v1 — should return v3 closest (similar direction), then v1 (exact)
        let results = idx.search(&v1, 3);
        assert_eq!(results.len(), 3);
        // First result should be symbol 100 (exact match, dist ~0)
        assert_eq!(results[0].0, 100);
        // v3 is closer to v1 than v2 is
        let d_v3 = results.iter().find(|(id, _)| *id == 300).map(|(_, d)| *d);
        let d_v2 = results.iter().find(|(id, _)| *id == 200).map(|(_, d)| *d);
        assert!(d_v3.unwrap() < d_v2.unwrap());
    }

    #[test]
    fn test_replace_on_duplicate_insert() {
        let _g = with_store_lock();
        let mut idx = CodeVectorIndex::new(64).unwrap();

        let v1 = make_unit_vec(64, 0);
        let v2 = make_unit_vec(64, 1);

        idx.insert(42, &v1).unwrap();
        assert_eq!(idx.len(), 1);

        // Insert same symbol ID with different vector — should replace
        idx.insert(42, &v2).unwrap();
        assert_eq!(idx.len(), 1); // still 1, not 2

        let results = idx.search(&v2, 1);
        assert_eq!(results[0].0, 42);
    }

    #[test]
    fn test_remove() {
        let _g = with_store_lock();
        let mut idx = CodeVectorIndex::new(64).unwrap();

        idx.insert(1, &make_unit_vec(64, 0)).unwrap();
        idx.insert(2, &make_unit_vec(64, 1)).unwrap();
        assert_eq!(idx.len(), 2);

        assert!(idx.remove(1));
        assert_eq!(idx.len(), 1);

        let results = idx.search(&make_unit_vec(64, 0), 5);
        assert!(results.iter().all(|(id, _)| *id != 1));
    }

    #[test]
    fn test_save_and_load() {
        let _g = with_store_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");

        {
            let mut idx = CodeVectorIndex::new(64).unwrap();
            idx.insert(10, &make_unit_vec(64, 0)).unwrap();
            idx.insert(20, &make_unit_vec(64, 1)).unwrap();
            idx.path = Some(path.clone());
            idx.save().unwrap();
        }

        let idx2 = CodeVectorIndex::load_or_create(&path, DEFAULT_DIMS).unwrap();
        assert_eq!(idx2.len(), 2);

        let results = idx2.search(&make_unit_vec(64, 0), 1);
        assert_eq!(results[0].0, 10);
    }

    #[test]
    fn test_vecq_backend_roundtrip() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        let mut idx = CodeVectorIndex::new(64).unwrap();

        idx.insert(1, &make_unit_vec(64, 0)).unwrap();
        idx.insert(2, &make_unit_vec(64, 1)).unwrap();
        assert_eq!(idx.len(), 2);

        // Replace semantics
        idx.insert(1, &make_unit_vec(64, 2)).unwrap();
        assert_eq!(idx.len(), 2);

        // Removal
        assert!(idx.remove(2));
        assert!(!idx.remove(2));
        assert_eq!(idx.len(), 1);

        let results = idx.search(&make_unit_vec(64, 2), 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
        // distance conversion: similarity ~1.0 -> distance ~0.0
        assert!(results[0].1 < 0.05, "dist={:?}", results[0].1);

        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn test_vecq_save_and_load_uses_own_extension() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch"); // caller names it; backend picks .vecq

        {
            let mut idx = CodeVectorIndex::new(64).unwrap();
            idx.insert(10, &make_unit_vec(64, 0)).unwrap();
            idx.path = Some(path.clone());
            idx.save().unwrap();
        }

        assert!(dir.path().join("test.vecq").exists());
        // Upstream gap (vecq#32): keys are not serialized, so reload rebuilds
        // fresh instead of serving a silently-empty index.
        let idx2 = CodeVectorIndex::load_or_create(&path, 64).unwrap();
        assert!(idx2.is_empty(), "reload must not serve a keyless index");
        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn vector_store_parse_is_lenient() {
        assert_eq!(VectorStoreKind::parse("vecq"), VectorStoreKind::Vecq);
        assert_eq!(VectorStoreKind::parse("VECQ "), VectorStoreKind::Vecq);
        assert_eq!(VectorStoreKind::parse("usearch"), VectorStoreKind::Usearch);
        assert_eq!(VectorStoreKind::parse(""), VectorStoreKind::Usearch);
        assert_eq!(VectorStoreKind::parse("bogus"), VectorStoreKind::Usearch);
    }

    #[test]
    fn test_cosine_distance_to_similarity() {
        assert!((cosine_distance_to_similarity(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(1.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(2.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(0.5) - 0.5).abs() < f32::EPSILON);
    }
}
