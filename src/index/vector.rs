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
    /// vecq quantized scan — pure Rust, deterministic, ~5x smaller.
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

/// vecq quantization width (`brain.vector_bits` in `.cora.yaml`).
///
/// Default is 4-bit + residual rescore: on cora's default hashing-trick
/// embeddings it measured the best recall@10 at 4-bit scan speed across
/// 1k/5k/13k-symbol indexes (recall study, 2026-08) — plain 5-bit, the
/// vecq-core default, trailed by ~4-7 points at every scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VectorBits {
    /// 4-bit base codes + second-pass residual rescoring (format v1.4).
    #[default]
    Residual,
    /// Plain 4-bit Lloyd-Max (max compression, lowest recall).
    B4,
    /// Plain 5-bit Lloyd-Max (vecq-core's out-of-the-box default).
    B5,
    /// Plain 6-bit Lloyd-Max (highest plain recall, slowest scan).
    B6,
}

impl VectorBits {
    /// Lenient like [`VectorStoreKind::parse`]: empty/unknown → Residual.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "4" => Self::B4,
            "5" => Self::B5,
            "6" => Self::B6,
            _ => Self::Residual, // "residual", "" and anything unknown
        }
    }

    fn create(&self, dims: usize) -> vecq_core::VecqIndex {
        match self {
            Self::Residual => vecq_core::VecqIndex::with_residual(dims, VECQ_SEED),
            Self::B4 => Self::create_plain(dims, 4),
            Self::B5 => Self::create_plain(dims, 5),
            Self::B6 => Self::create_plain(dims, 6),
        }
    }

    fn create_plain(dims: usize, bits: u8) -> vecq_core::VecqIndex {
        let mut index = vecq_core::VecqIndex::new(dims, VECQ_SEED);
        index.set_bits(bits);
        index
    }

    /// Whether an on-disk index was built at this width. A mismatch triggers
    /// one rebuild so a config change actually takes effect instead of
    /// silently serving the old width forever.
    fn matches(&self, index: &vecq_core::VecqIndex) -> bool {
        match self {
            Self::Residual => index.is_residual(),
            Self::B4 => !index.is_residual() && index.bits() == 4,
            Self::B5 => !index.is_residual() && index.bits() == 5,
            Self::B6 => !index.is_residual() && index.bits() == 6,
        }
    }
}

static VECTOR_STORE: LazyLock<std::sync::RwLock<VectorStoreKind>> =
    LazyLock::new(|| std::sync::RwLock::new(VectorStoreKind::Usearch));

static VECTOR_BITS: LazyLock<std::sync::RwLock<VectorBits>> =
    LazyLock::new(|| std::sync::RwLock::new(VectorBits::default()));

/// Select the physical vector store used by NEW indexes (process-wide).
/// Existing on-disk indexes keep their own format via file extension.
pub fn set_vector_store(kind: VectorStoreKind) {
    *VECTOR_STORE.write().unwrap() = kind;
}

pub fn current_vector_store() -> VectorStoreKind {
    *VECTOR_STORE.read().unwrap()
}

/// Select the vecq quantization width used by NEW indexes (process-wide).
pub fn set_vector_bits(bits: VectorBits) {
    *VECTOR_BITS.write().unwrap() = bits;
}

pub fn current_vector_bits() -> VectorBits {
    *VECTOR_BITS.read().unwrap()
}

/// Apply `brain.vector_store` + `brain.vector_bits` from a loaded config (any
/// process entry that touches Brain search/embedding must call this before
/// opening the index, or `.vecq`/`.usearch` files silently diverge between
/// runs).
pub fn apply_config_store(config: Option<&crate::config::schema::Config>) {
    let (kind, bits) = config
        .map(|c| {
            (
                VectorStoreKind::parse(&c.brain.vector_store),
                VectorBits::parse(&c.brain.vector_bits),
            )
        })
        .unwrap_or_default();
    set_vector_store(kind);
    set_vector_bits(bits);
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
                VectorStoreKind::Vecq => Inner::Vecq(Box::new(current_vector_bits().create(dims))),
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

        let bits = current_vector_bits();
        let mut index = bits.create(dims);
        let mut dirty = false;
        if !buffer.is_empty() {
            match vecq_core::VecqIndex::from_bytes(&buffer) {
                Ok(loaded)
                    if loaded.dim() == dims && bits.matches(&loaded) && vecq_has_keys(&loaded) =>
                {
                    index = loaded;
                }
                // Dim mismatch (e.g. rebuilt with/without pretrained-embed):
                // keeping the loaded dims would panic on the next insert.
                Ok(loaded) if loaded.dim() != dims => {
                    tracing::warn!(
                        "vecq index dims {} != current {dims} — recreating; \
                         `cora index` will re-embed all symbols",
                        loaded.dim()
                    );
                    dirty = true;
                }
                Ok(loaded) if !bits.matches(&loaded) => {
                    tracing::warn!(
                        "vecq index width ({}) != configured brain.vector_bits ({bits:?}) — \
                         recreating; `cora index` will re-embed all symbols",
                        if loaded.is_residual() {
                            "residual".to_string()
                        } else {
                            format!("{}-bit", loaded.bits())
                        }
                    );
                    dirty = true;
                }
                // Legacy vecq-core 0.2.x file (vecq#32): parses fine but
                // carries no keyed-slot table, and cora's symbol ids live in
                // the key map — searching it would silently return nothing.
                Ok(_) => {
                    tracing::warn!(
                        "vecq file predates keyed persistence (vecq#32) — \
                         recreating index; `cora index` will re-embed all symbols"
                    );
                    dirty = true;
                }
                Err(e) => {
                    tracing::warn!(
                        "vecq index unreadable ({e}) — recreating; \
                         `cora index` will re-embed all symbols"
                    );
                    dirty = true;
                }
            }
        }

        Ok(Self {
            inner: Inner::Vecq(Box::new(index)),
            path: Some(path.to_path_buf()),
            dirty,
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

/// cora only inserts via `add_keyed`, so every live slot of a healthy index
/// carries a key. Files written by vecq-core 0.2.x (before format v1.3)
/// load with vectors but no key table — searching them would silently
/// return nothing (vecq#32).
fn vecq_has_keys(index: &vecq_core::VecqIndex) -> bool {
    index.is_empty() || (0..index.slots()).any(|s| index.key_of(s).is_some())
}

/// True when the `.vecq` file exists but cannot serve keyed search after
/// reload — legacy vecq#32 file, unreadable, built with different dims, or
/// at a different width than the configured `brain.vector_bits`. Mirrors
/// exactly what [`CodeVectorIndex::load_or_create`] will decide, so
/// `cora index` can force a re-embed when the index is about to be rebuilt.
pub fn vecq_file_needs_rebuild(path: &Path, dims: usize) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return false, // no file / unreadable — nothing to rebuild
    };
    if bytes.is_empty() {
        return false;
    }
    match vecq_core::VecqIndex::from_bytes(&bytes) {
        Ok(loaded) => {
            loaded.dim() != dims
                || !current_vector_bits().matches(&loaded)
                || !vecq_has_keys(&loaded)
        }
        Err(_) => true,
    }
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
    fn test_vecq_backend_roundtrip_all_widths() {
        for bits in [
            VectorBits::Residual,
            VectorBits::B4,
            VectorBits::B5,
            VectorBits::B6,
        ] {
            let _g = with_store_lock();
            set_vector_store(VectorStoreKind::Vecq);
            set_vector_bits(bits);
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

            set_vector_bits(VectorBits::default());
            set_vector_store(VectorStoreKind::Usearch);
        }
    }

    #[test]
    fn test_vecq_save_and_load_restores_keys() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        set_vector_bits(VectorBits::default()); // residual
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch"); // caller names it; backend picks .vecq

        {
            let mut idx = CodeVectorIndex::new(64).unwrap();
            idx.insert(10, &make_unit_vec(64, 0)).unwrap();
            idx.insert(20, &make_unit_vec(64, 1)).unwrap();
            idx.path = Some(path.clone());
            idx.save().unwrap();
        }

        assert!(dir.path().join("test.vecq").exists());
        // vecq-core 0.3.0 keyed persistence (vecq#32 closed): symbol-id keys
        // survive reload, so a fresh process serves the index as-is.
        let idx2 = CodeVectorIndex::load_or_create(&path, 64).unwrap();
        assert_eq!(idx2.len(), 2, "keyed map must survive reload");
        assert!(!idx2.is_dirty(), "healthy keyed file loads clean");

        let results = idx2.search(&make_unit_vec(64, 0), 1);
        assert_eq!(results[0].0, 10);
        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn test_vecq_legacy_keyless_file_rebuilds() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        // The simulated legacy file is plain 5-bit; match that width so the
        // load hits the keyless arm rather than the width-mismatch arm.
        set_vector_bits(VectorBits::B5);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");
        let vecq_path = dir.path().join("test.vecq");

        // Simulate a vecq-core 0.2.x file (vecq#32): vectors serialized
        // without a keyed-slot table, which is what unkeyed `add()` produces.
        let mut legacy = vecq_core::VecqIndex::new(64, VECQ_SEED);
        legacy.add(&make_unit_vec(64, 0));
        std::fs::write(&vecq_path, legacy.to_bytes()).unwrap();

        let idx = CodeVectorIndex::load_or_create(&path, 64).unwrap();
        assert!(idx.is_empty(), "keyless file must not be served");
        assert!(idx.is_dirty(), "keyless file must trigger a rebuild");
        set_vector_bits(VectorBits::default());
        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn test_vecq_width_switch_rebuilds() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        set_vector_bits(VectorBits::default()); // residual
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");

        {
            let mut idx = CodeVectorIndex::new(64).unwrap();
            idx.insert(10, &make_unit_vec(64, 0)).unwrap();
            idx.path = Some(path.clone());
            idx.save().unwrap();
        }

        // A healthy keyed index must NOT be kept when the configured width
        // changed — the rebuild makes brain.vector_bits actually take effect.
        set_vector_bits(VectorBits::B5);
        let idx = CodeVectorIndex::load_or_create(&path, 64).unwrap();
        assert!(idx.is_empty(), "width-mismatched file must not be served");
        assert!(idx.is_dirty(), "width change must trigger a rebuild");
        set_vector_bits(VectorBits::default());
        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn test_vecq_corrupt_file_rebuilds() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");
        std::fs::write(dir.path().join("test.vecq"), vec![0u8; 64]).unwrap();

        let idx = CodeVectorIndex::load_or_create(&path, 64).unwrap();
        assert!(idx.is_empty());
        assert!(idx.is_dirty());
        set_vector_store(VectorStoreKind::Usearch);
    }

    #[test]
    fn test_vecq_dim_mismatch_rebuilds() {
        let _g = with_store_lock();
        set_vector_store(VectorStoreKind::Vecq);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");

        {
            let mut idx = CodeVectorIndex::new(64).unwrap();
            idx.insert(10, &make_unit_vec(64, 0)).unwrap();
            idx.path = Some(path.clone());
            idx.save().unwrap();
        }

        let idx = CodeVectorIndex::load_or_create(&path, 128).unwrap();
        assert!(idx.is_empty());
        assert!(idx.is_dirty());
        assert_eq!(idx.dims(), 128);
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
    fn vector_bits_parse_is_lenient() {
        assert_eq!(VectorBits::parse("residual"), VectorBits::Residual);
        assert_eq!(VectorBits::parse(" RESIDUAL "), VectorBits::Residual);
        assert_eq!(VectorBits::parse("4"), VectorBits::B4);
        assert_eq!(VectorBits::parse("5"), VectorBits::B5);
        assert_eq!(VectorBits::parse("6"), VectorBits::B6);
        assert_eq!(VectorBits::parse(""), VectorBits::Residual);
        assert_eq!(VectorBits::parse("bogus"), VectorBits::Residual);
    }

    #[test]
    fn test_cosine_distance_to_similarity() {
        assert!((cosine_distance_to_similarity(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(1.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(2.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(0.5) - 0.5).abs() < f32::EPSILON);
    }
}
