//! Vector files: one quantized (vecq) file per project and embedding model.
//!
//! A project's vectors live in `<data dir>/vectors/<project id>.<model tag>.vecq`,
//! keyed by symbol id. The file is rebuilt from the project's symbols after every
//! index run that changed them and replaced atomically (write to a temporary
//! file, then rename), so it never holds a vector of a symbol that is gone and
//! a reader never sees half a file. A search loads only the current project's
//! file and scans it: no other project's vectors are read, filtered or
//! returned, and nothing is shared between projects or processes except the
//! rename.
//!
//! Measured against one shared HNSW index on 176 projects (127,400 vectors),
//! this layout keeps retrieval at the exact ceiling, uses 4.3x less disk, loads
//! in 0.1 ms and needs no resident memory for an idle project (#750).

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;
use vecq_core::VecqIndex;

/// Seed of the quantizer (fixed so a rebuild of the same data is identical).
const SEED: u64 = 42;
const EXT: &str = "vecq";

/// Directory holding every project's vector file.
pub fn vectors_dir() -> PathBuf {
    crate::data_dir::cora_data_dir().join("vectors")
}

/// Vector file of `project_id` for the model named `tag`.
pub fn project_file(project_id: i64, tag: &str) -> PathBuf {
    vectors_dir().join(format!("{project_id}.{tag}.{EXT}"))
}

/// One project's vectors.
pub struct ProjectVectors {
    index: VecqIndex,
}

/// What reading a project's vector file found.
pub enum Loaded {
    /// The file is there and usable.
    Ready(Arc<ProjectVectors>),
    /// No file: the project was never embedded with this model.
    Missing,
    /// The file was built with other dimensions than the active model.
    DimsMismatch { file: usize, wanted: usize },
    /// The file cannot be read (corrupt, or a format from an older vecq).
    Unreadable(String),
}

impl ProjectVectors {
    /// Build a project's vectors from `(symbol id, vector)` pairs.
    pub fn build(dims: usize, items: impl IntoIterator<Item = (i64, Vec<f32>)>) -> Self {
        let mut index = VecqIndex::with_residual(dims, SEED);
        for (id, v) in items {
            index.add_keyed(id as u64, &v);
        }
        Self { index }
    }

    /// Write the file atomically: a temporary file in the same directory, then a rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("vector file has no directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let tmp = path.with_extension(format!("{EXT}.tmp-{}", std::process::id()));
        std::fs::write(&tmp, self.index.to_bytes())
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    }

    /// The `k` nearest symbols of the project as `(symbol id, similarity)`,
    /// best first.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(i64, f32)> {
        if self.index.is_empty() || query.len() != self.index.dim() {
            return Vec::new();
        }
        self.index
            .search_keyed(query, k.max(1))
            .into_iter()
            .map(|(key, sim)| (key as i64, sim))
            .collect()
    }

    /// Number of vectors.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Number of dimensions.
    pub fn dim(&self) -> usize {
        self.index.dim()
    }
}

/// True when every live slot carries a key. Files written by vecq-core 0.2.x
/// parse but have no key table, so a search would return nothing.
fn has_keys(index: &VecqIndex) -> bool {
    index.is_empty() || (0..index.slots()).any(|s| index.key_of(s).is_some())
}

/// Read a vector file without caching.
fn read_file(path: &Path, dims: usize) -> Loaded {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::Missing,
        Err(e) => return Loaded::Unreadable(e.to_string()),
    };
    match VecqIndex::from_bytes(&bytes) {
        Ok(index) if index.dim() != dims => Loaded::DimsMismatch {
            file: index.dim(),
            wanted: dims,
        },
        Ok(index) if !has_keys(&index) => {
            Loaded::Unreadable("a vector file from before keyed storage".to_string())
        }
        Ok(index) => Loaded::Ready(Arc::new(ProjectVectors { index })),
        Err(e) => Loaded::Unreadable(e.to_string()),
    }
}

type Entry = (SystemTime, u64, Arc<ProjectVectors>);

/// Files loaded by this process, reloaded when the file changes. A long-lived
/// process (the MCP server, `cora watch`) therefore sees a re-index.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Load a project's vector file, from this process's cache when it has not changed.
pub fn load_cached(path: &Path, dims: usize) -> Loaded {
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
    if let Some((mtime, len)) = stamp {
        if let Some((m, l, v)) = CACHE.lock().unwrap().get(path) {
            if *m == mtime && *l == len && v.dim() == dims {
                return Loaded::Ready(Arc::clone(v));
            }
        }
    }
    let loaded = read_file(path, dims);
    if let (Loaded::Ready(v), Some((mtime, len))) = (&loaded, stamp) {
        CACHE
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), (mtime, len, Arc::clone(v)));
    }
    loaded
}

/// Forget a cached file (after it was rewritten or deleted).
pub fn forget(path: &Path) {
    CACHE.lock().unwrap().remove(path);
}

/// Delete the vector files of a project: every model's, or all but `keep`.
/// Used when a project is removed or rebuilt from scratch, and to drop the file
/// of a model the project no longer uses.
pub fn remove_project_files(project_id: i64, keep: Option<&str>) {
    let Ok(rd) = std::fs::read_dir(vectors_dir()) else {
        return;
    };
    let prefix = format!("{project_id}.");
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(tag) = rest.strip_suffix(&format!(".{EXT}")) else {
            continue;
        };
        if keep.is_none_or(|k| k != tag) {
            forget(&entry.path());
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Delete the files of the former shared index (`cora_index.*`). Nothing reads
/// them any more; every project is rebuilt into its own file on its next index run.
pub fn remove_legacy_shared_index() {
    let dir = crate::data_dir::cora_data_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("cora_index.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(x: &[f32]) -> Vec<f32> {
        let n = x.iter().map(|v| v * v).sum::<f32>().sqrt();
        x.iter().map(|v| v / n).collect()
    }

    fn sample() -> ProjectVectors {
        ProjectVectors::build(
            4,
            vec![
                (10, unit(&[1.0, 0.0, 0.0, 0.0])),
                (11, unit(&[0.0, 1.0, 0.0, 0.0])),
                (12, unit(&[0.9, 0.1, 0.0, 0.0])),
                (13, unit(&[0.0, 0.0, 1.0, 0.0])),
            ],
        )
    }

    #[test]
    fn the_nearest_symbols_come_back_best_first_with_their_ids() {
        let v = sample();
        let hits = v.search(&unit(&[1.0, 0.05, 0.0, 0.0]), 2);
        let ids: Vec<i64> = hits.iter().map(|h| h.0).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&10) && ids.contains(&12), "{ids:?}");
        assert!(hits[0].1 >= hits[1].1);
    }

    #[test]
    fn a_saved_file_loads_with_the_same_ids_and_a_rebuild_replaces_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("vectors").join("7.test.vecq");
        sample().save(&path).unwrap();
        assert!(path.exists());
        // no temporary file is left behind
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["7.test.vecq"]);

        let Loaded::Ready(v) = read_file(&path, 4) else {
            panic!("expected a usable file");
        };
        assert_eq!(v.len(), 4);
        let ids: Vec<i64> = v
            .search(&unit(&[0.0, 0.0, 1.0, 0.0]), 1)
            .iter()
            .map(|h| h.0)
            .collect();
        assert_eq!(ids, [13]);

        // rebuilding with different symbols leaves no trace of the old ones
        ProjectVectors::build(4, vec![(99, unit(&[0.0, 0.0, 1.0, 0.0]))])
            .save(&path)
            .unwrap();
        let Loaded::Ready(v) = read_file(&path, 4) else {
            panic!("expected a usable file");
        };
        assert_eq!(v.len(), 1);
        assert_eq!(v.search(&unit(&[0.0, 0.0, 1.0, 0.0]), 5)[0].0, 99);
    }

    #[test]
    fn a_missing_wrong_sized_or_corrupt_file_is_reported_not_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("1.m.vecq");
        assert!(matches!(read_file(&path, 4), Loaded::Missing));
        sample().save(&path).unwrap();
        assert!(matches!(
            read_file(&path, 8),
            Loaded::DimsMismatch { file: 4, wanted: 8 }
        ));
        std::fs::write(&path, b"not a vecq file").unwrap();
        assert!(matches!(read_file(&path, 4), Loaded::Unreadable(_)));
    }

    #[test]
    fn the_cache_sees_a_rewritten_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("2.m.vecq");
        sample().save(&path).unwrap();
        let Loaded::Ready(first) = load_cached(&path, 4) else {
            panic!("ready");
        };
        assert_eq!(first.len(), 4);
        // a re-index rewrites the file with another size: the next load sees it
        ProjectVectors::build(4, vec![(1, unit(&[1.0, 0.0, 0.0, 0.0]))])
            .save(&path)
            .unwrap();
        let Loaded::Ready(second) = load_cached(&path, 4) else {
            panic!("ready");
        };
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn query_of_the_wrong_size_returns_nothing_instead_of_panicking() {
        assert!(sample().search(&[1.0, 0.0], 3).is_empty());
    }

    /// Unit tests get an isolated data directory (`data_dir::codecora_home`).
    #[test]
    fn removing_project_files_matches_the_project_id_exactly_and_can_keep_a_model() {
        let dir = vectors_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["9001.a.vecq", "9001.b.vecq", "90010.a.vecq", "9002.a.vecq"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_project_files(9001, Some("a"));
        assert!(dir.join("9001.a.vecq").exists(), "the kept model stays");
        assert!(
            !dir.join("9001.b.vecq").exists(),
            "another model is removed"
        );
        assert!(
            dir.join("90010.a.vecq").exists(),
            "project 90010 is not project 9001"
        );
        assert!(dir.join("9002.a.vecq").exists());
        remove_project_files(9001, None);
        assert!(!dir.join("9001.a.vecq").exists());
        assert!(dir.join("90010.a.vecq").exists());
    }
}
