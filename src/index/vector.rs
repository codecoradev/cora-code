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
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const USEARCH_EXT: &str = "usearch";

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
    index: Index,
    /// Maps usearch integer key → symbol database ID.
    key_to_symbol: HashMap<u64, i64>,
    /// Maps symbol database ID → usearch integer key.
    symbol_to_key: HashMap<i64, u64>,
    next_key: u64,
    path: Option<PathBuf>,
    dirty: bool,
    _lock_file: Option<File>,
}

impl CodeVectorIndex {
    /// Create a new empty in-memory index.
    pub fn new(dims: usize) -> Result<Self> {
        let index = create_usearch_index(dims)?;
        Ok(Self {
            index,
            key_to_symbol: HashMap::new(),
            symbol_to_key: HashMap::new(),
            next_key: 0,
            path: None,
            dirty: false,
            _lock_file: None,
        })
    }

    /// Load from disk, or create empty if not exists.
    /// Acquires exclusive file lock for cross-process safety.
    pub fn load_or_create(path: &Path, dims: usize) -> Result<Self> {
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
            index,
            key_to_symbol,
            symbol_to_key,
            next_key,
            path: None,
            dirty: false,
            _lock_file: None,
        })
    }

    /// Save index and key mappings to disk.
    pub fn save(&mut self) -> Result<()> {
        if let Some(ref path) = self.path {
            let buf_len = self.index.serialized_length();
            let mut buffer = vec![0u8; buf_len];
            self.index
                .save_to_buffer(&mut buffer)
                .context("save usearch to buffer")?;

            let tmp_path = path.with_extension(format!("{USEARCH_EXT}.tmp"));
            std::fs::write(&tmp_path, &buffer).context("write temp usearch index")?;
            std::fs::rename(&tmp_path, path).context("rename temp usearch")?;

            // Save key mapping sidecar
            let mapping_path = path.with_extension("keys");
            let mut lines = Vec::new();
            for (&key, &sym_id) in &self.key_to_symbol {
                lines.push(format!("{key}\t{sym_id}"));
            }
            atomic_write(&mapping_path, lines.join("\n").as_bytes())?;

            self.dirty = false;
        }
        Ok(())
    }

    /// Insert a symbol embedding. If symbol ID exists, replaces it.
    pub fn insert(&mut self, symbol_id: i64, embedding: &[f32]) -> Result<()> {
        // Remove old entry if exists
        if let Some(&old_key) = self.symbol_to_key.get(&symbol_id) {
            self.key_to_symbol.remove(&old_key);
            self.index
                .remove(old_key)
                .context("remove old usearch entry")?;
        }

        let key = self.next_key;
        self.next_key += 1;
        self.key_to_symbol.insert(key, symbol_id);
        self.symbol_to_key.insert(symbol_id, key);

        // Auto-reserve if at capacity
        if self.index.size() >= self.index.capacity() {
            let new_cap = (self.index.capacity() + 1024).max(1024);
            self.index
                .reserve(new_cap)
                .context("reserve usearch capacity")?;
        }

        self.index
            .add(key, embedding)
            .context("insert into usearch")?;

        self.dirty = true;
        Ok(())
    }

    /// Remove a symbol by database ID. Incremental, no rebuild.
    pub fn remove(&mut self, symbol_id: i64) -> bool {
        if let Some(&key) = self.symbol_to_key.get(&symbol_id) {
            self.key_to_symbol.remove(&key);
            self.symbol_to_key.remove(&symbol_id);
            if let Err(e) = self.index.remove(key) {
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
        if self.index.size() == 0 {
            return Vec::new();
        }
        let count = k.max(1);
        let results = match self.index.search(query, count) {
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
            .filter_map(|(key, dist)| self.key_to_symbol.get(key).map(|&id| (id, *dist)))
            .collect()
    }

    /// Number of vectors in the index.
    pub fn len(&self) -> usize {
        self.index.size()
    }

    /// Embedding dimensionality.
    pub fn dims(&self) -> usize {
        self.index.dimensions()
    }

    /// Whether the index has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.index.size() == 0
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

    #[test]
    fn test_empty_search() {
        let idx = CodeVectorIndex::new(768).unwrap();
        assert!(idx.is_empty());
        let results = idx.search(&[0.0f32; 768], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_insert_and_search() {
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
    fn test_cosine_distance_to_similarity() {
        assert!((cosine_distance_to_similarity(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(1.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(2.0) - 0.0).abs() < f32::EPSILON);
        assert!((cosine_distance_to_similarity(0.5) - 0.5).abs() < f32::EPSILON);
    }
}
