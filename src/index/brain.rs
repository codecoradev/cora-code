//! Brain Mode — hybrid search combining FTS5 + usearch vectors + graph proximity.
//!
//! RRF fusion (k=60) merges 3 signal sources into ranked results.
//! Pattern adopted from uteke `doc_search_hybrid()`.
//!
//! Embedding backend is selected at compile time:
//! - `pretrained-embed` feature → nomic-embed-code 768-dim vectors
//! - default → hashing-trick 256-dim vectors
//!
//! A dimension mismatch between an existing on-disk index and the current
//! compile-time backend triggers a warning and advises re-indexing.

use crate::embed::{active_dims, active_provider_name, embed_code_dispatch};
use crate::index::symbols::SymbolQuery;
use crate::index::vector::{CodeVectorIndex, cosine_distance_to_similarity};
use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, RwLock};

/// RRF constant (standard value from Cormack et al. 2009).
const RRF_K: f32 = 60.0;

// ── Process-lifetime caches ─────────────────────────────────────────────

/// Cached vector index — loaded once from disk, reused for all searches.
/// Write-locked during embed_project (indexing), read-locked during search.
static VECTOR_CACHE: LazyLock<RwLock<Option<CodeVectorIndex>>> =
    LazyLock::new(|| RwLock::new(None));

/// Cached project → symbol ID sets, used to filter vector results without
/// a full DB scan per query. Invalidated when a project is re-indexed.
static PROJECT_ID_CACHE: LazyLock<RwLock<HashMap<i64, HashSet<i64>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// A brain search result with provenance information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrainResult {
    pub symbol_id: i64,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: i64,
    pub signature: String,
    /// Fused RRF score (higher = better).
    pub score: f32,
    /// Which signals contributed: fts, vector, graph.
    pub signals: Vec<String>,
}

/// Path to the usearch vector index file.
fn vector_index_path() -> std::path::PathBuf {
    crate::data_dir::cora_data_dir().join("cora_index.usearch")
}

/// Check if an existing on-disk vector index has compatible dimensions.
///
/// If the index was created with different dimensions (e.g. 256d hashing-trick
/// but now running with 768d pretrained), logs a warning advising re-indexing.
///
/// The index file contains usearch binary data — we can't easily read the
/// dimensionality without loading it. Instead, we check the `embedding_dims`
/// column in the projects table, which is written during embedding.
fn check_dimension_compat(vi_path: &std::path::Path, expected_dims: usize) {
    // Attempt to load the index just to check its dimensions.
    // If it fails (empty, corrupt, etc.), we'll create fresh — no warning needed.
    let Ok(file) = std::fs::File::open(vi_path) else {
        return;
    };
    let Ok(metadata) = file.metadata() else {
        return;
    };
    if metadata.len() == 0 {
        return; // Empty file — will create fresh
    }

    // Try to load and check dims
    let result = std::panic::catch_unwind(|| {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        let _ = file.seek(SeekFrom::Start(0));
        let mut buffer = Vec::new();
        let _ = file.read_to_end(&mut buffer);
        if buffer.is_empty() {
            return None;
        }
        let index = usearch::Index::restore_from_buffer(&buffer).ok()?;
        Some(index.dimensions())
    });

    match result {
        Ok(Some(disk_dims)) if disk_dims != expected_dims => {
            tracing::warn!(
                "⚠ Vector index dimension mismatch: on-disk={disk_dims}, current={expected_dims} ({})",
                active_provider_name()
            );
            tracing::warn!(
                "  The vector index was built with a different embedding backend. \
                 Run `cora index` to re-index with the current backend."
            );
            // Delete the stale index so embed_project creates a fresh one
            if let Err(e) = std::fs::remove_file(vi_path) {
                tracing::warn!("  Failed to remove stale index: {e}");
            } else {
                let keys_path = vi_path.with_extension("keys");
                let _ = std::fs::remove_file(&keys_path);
                tracing::info!("  Removed stale vector index — will create fresh on next index");
            }
        }
        Ok(Some(_)) => {
            // Dimensions match — all good
        }
        _ => {
            // Couldn't read dimensions (corrupt, unsupported, etc.)
            // embed_project will handle creation
        }
    }
}

/// Embed all symbols for a project into the vector index.
///
/// Uses the embedding backend selected at runtime via [`resolve_backend`]:
/// - `"pretrained"` → nomic-embed-code 768-dim vectors
/// - `"hashing"` → hashing-trick 256-dim vectors
/// - `"auto"` → best available
///
/// **Incremental**: Only symbols whose `name + signature` fingerprint has
/// changed since the last embed are re-embedded. This dramatically reduces
/// embedding time when a single file is modified (e.g. 10 changed symbols
/// out of 1100 total).
///
/// Detects dimension mismatch between existing on-disk index and current
/// backend, warning the user to re-index if dimensions changed.
/// Call after `index_project`.
pub fn embed_project(conn: &Connection, project_id: i64) -> Result<usize> {
    let vi_path = vector_index_path();
    let active = active_dims();

    // Check for existing index with mismatched dimensions (e.g. user rebuilt
    // with/without pretrained-embed feature). Warn but continue — the index
    // will be corrupted for searches until re-indexed.
    if vi_path.exists() {
        check_dimension_compat(&vi_path, active);
    }

    // Acquire write lock — block searches while embedding.
    // This is fine because embedding only happens during `cora index`,
    // never during `cora brain_search`.
    let mut cache = VECTOR_CACHE.write().unwrap();

    // Load or create the vector index (reuses cache if available)
    let vi = if let Some(ref mut cached) = *cache {
        cached
    } else {
        let vi = CodeVectorIndex::load_or_create(&vi_path, active).context("load vector index")?;
        *cache = Some(vi);
        cache.as_mut().unwrap()
    };

    // ── Self-heal after vecq reload-discard (#542, vecq#32) ──────────
    // A vecq index reloaded from disk is rebuilt empty (upstream lacks key
    // serialization) and arrives dirty. Stored fingerprints would skip all
    // unchanged symbols, permanently excluding them from vector search.
    // Clear fingerprints so this run re-embeds every symbol in the project.
    if vi.is_dirty() && vi.is_empty() {
        let cleared = conn.execute(
            "UPDATE symbols SET embed_fingerprint = NULL WHERE project_id = ?1",
            rusqlite::params![project_id],
        )?;
        tracing::info!(
            cleared,
            "vector index rebuilt from empty (vecq reload) — re-embedding all symbols"
        );
    }

    // ── Incremental: fetch stored fingerprints ──────────────────────
    // Only re-embed symbols whose name+signature has changed.
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, signature, embed_fingerprint \
         FROM symbols WHERE project_id = ?1",
    )?;
    let rows: Vec<(i64, String, String, String, Option<String>)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Compute current fingerprints and filter to only changed symbols
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let changed: Vec<(i64, String)> = rows
        .iter()
        .filter_map(|(sym_id, name, _kind, signature, stored_fp)| {
            let text = if signature.is_empty() || signature == name {
                name.clone()
            } else {
                format!("{name} {signature}")
            };
            let mut hasher = DefaultHasher::new();
            text.hash(&mut hasher);
            let current_fp = format!("{:016x}", hasher.finish());

            if stored_fp.as_deref() == Some(&current_fp) {
                None // unchanged — skip
            } else {
                Some((*sym_id, text))
            }
        })
        .collect();

    let total_symbols = rows.len();
    let skipped = total_symbols - changed.len();
    if skipped > 0 {
        tracing::info!(
            "Incremental embed: {total_symbols} total, {skipped} unchanged (skipped), {} changed (re-embedding)",
            changed.len()
        );
    }

    // ── Parallel embedding computation (Rayon) ─────────────────────────
    // embed_code_dispatch is pure + CPU-bound. usearch insert is serial.
    let t_compute = std::time::Instant::now();
    let embedded: Vec<(i64, Vec<f32>)> = changed
        .par_iter()
        .map(|(sym_id, text)| {
            let vec = embed_code_dispatch(text);
            (*sym_id, vec)
        })
        .collect();
    let compute_ms = t_compute.elapsed().as_millis();

    // ── Serial usearch insert ────────────────────────────────────────
    let t_insert = std::time::Instant::now();
    let mut count = 0;
    let mut new_ids: HashSet<i64> = HashSet::with_capacity(rows.len());
    // Populate new_ids with ALL symbol IDs for this project (for search filtering)
    for (sym_id, _, _, _, _) in &rows {
        new_ids.insert(*sym_id);
    }
    for (sym_id, vec) in &embedded {
        vi.insert(*sym_id, vec).context("insert symbol embedding")?;
        count += 1;
    }
    let insert_ms = t_insert.elapsed().as_millis();

    tracing::debug!(
        "embed_compute={}ms, usearch_insert={}ms, re-embedded={}, total={}, skipped={}, dims={}, provider={}",
        compute_ms,
        insert_ms,
        count,
        total_symbols,
        skipped,
        active,
        active_provider_name()
    );

    if vi.is_dirty() {
        vi.save().context("save vector index")?;
    }

    // ── Update fingerprints for embedded symbols ─────────────────────
    let mut update_fp = conn.prepare("UPDATE symbols SET embed_fingerprint = ?2 WHERE id = ?1")?;
    for (sym_id, text) in &changed {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let fp = format!("{:016x}", hasher.finish());
        update_fp.execute(rusqlite::params![sym_id, fp])?;
    }

    // Cache project → symbol IDs for fast search-time filtering
    PROJECT_ID_CACHE
        .write()
        .unwrap()
        .insert(project_id, new_ids);

    // Determine tier label
    let provider = active_provider_name();
    let tier = if provider.contains("pretrained") {
        "pretrained"
    } else {
        "static"
    };
    conn.execute(
        "UPDATE projects SET embedding_tier = ?3, embedding_dims = ?1, \
         embedding_provider = ?4, last_embedded_at = datetime('now') WHERE id = ?2",
        rusqlite::params![active, project_id, tier, provider],
    )?;

    tracing::info!(
        "Embedded {count}/{total_symbols} symbols for project {project_id} ({skipped} unchanged, provider={provider}, dims={active})",
    );
    Ok(count)
}

/// Hybrid brain search: FTS5 + usearch KNN + graph proximity → RRF fusion.
pub fn brain_search(
    conn: &Connection,
    project_id: i64,
    query: &str,
    limit: usize,
) -> Result<Vec<BrainResult>> {
    let limit = limit.min(50);
    let fetch_limit = limit * 2;

    let fts_hits = fts5_search(conn, project_id, query, fetch_limit);
    let vec_hits = vector_search(conn, project_id, query, fetch_limit);
    let graph_hits = graph_proximity_search(conn, project_id, &fts_hits, fetch_limit);

    // ── RRF Fusion ──────────────────────────────────────────────────────
    let mut fused: HashMap<i64, (f32, Vec<String>)> = HashMap::new();

    for (rank, (id, _score)) in fts_hits.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank as f32 + 1.0));
        let entry = fused.entry(*id).or_insert((0.0, Vec::new()));
        entry.0 += rrf;
        entry.1.push("fts".into());
    }

    for (rank, (id, _sim)) in vec_hits.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank as f32 + 1.0));
        let entry = fused.entry(*id).or_insert((0.0, Vec::new()));
        entry.0 += rrf;
        entry.1.push("vector".into());
    }

    for (rank, (id, _depth)) in graph_hits.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank as f32 + 1.0));
        let entry = fused.entry(*id).or_insert((0.0, Vec::new()));
        entry.0 += rrf;
        entry.1.push("graph".into());
    }

    let mut ranked: Vec<_> = fused.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.0
            .partial_cmp(&a.1.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit);

    // Batch fetch all symbol rows in a single query (was N+1 individual queries)
    let results = batch_get_symbols(conn, &ranked);

    Ok(results)
}

// ── Signal sources ───────────────────────────────────────────────────────

/// FTS5 keyword search → (symbol_id, rank_score) pairs.
fn fts5_search(conn: &Connection, project_id: i64, query: &str, limit: usize) -> Vec<(i64, f64)> {
    let sq = SymbolQuery::text(query);
    match crate::index::search(conn, project_id, &sq) {
        Ok(results) => results
            .into_iter()
            .take(limit)
            .map(|r| (r.symbol.id, r.score))
            .collect(),
        Err(e) => {
            tracing::warn!("FTS5 search error: {e}");
            Vec::new()
        }
    }
}

/// True when the on-disk vector index exists but will be discarded on reload —
/// i.e. vecq backend with a non-empty `.vecq` file that cannot restore its
/// key map (vecq#32). `cora index` uses this to force a re-embed even when
/// no files changed, so the vector signal heals instead of staying empty.
pub fn vector_index_needs_rebuild() -> bool {
    if crate::index::vector::current_vector_store() != crate::index::vector::VectorStoreKind::Vecq {
        return false;
    }
    let path = vector_index_path().with_extension("vecq");
    std::fs::metadata(&path)
        .map(|m| m.len() > 24)
        .unwrap_or(false)
}

/// usearch vector search → (symbol_id, cosine_similarity) pairs, filtered to project.
/// Uses cached vector index and cached project ID set — no disk I/O per query.
fn vector_search(conn: &Connection, project_id: i64, query: &str, limit: usize) -> Vec<(i64, f32)> {
    // Read-lock the cached vector index — no disk load
    let cache = VECTOR_CACHE.read().unwrap();
    let vi = match cache.as_ref() {
        Some(v) if !v.is_empty() => v,
        _ => return Vec::new(),
    };

    let vec = embed_code_dispatch(query);

    // Over-fetch to compensate for post-filter by project_id.
    let over_fetch = (limit * 5).max(50);
    let raw = vi.search(&vec, over_fetch);

    // Use cached project ID set — avoids full DB scan per query.
    // Falls back to DB query only on cache miss (first search after process start).
    let project_ids = {
        let cache = PROJECT_ID_CACHE.read().unwrap();
        cache.get(&project_id).cloned()
    };

    let project_ids = match project_ids {
        Some(ids) => ids,
        None => {
            // Cache miss — load from DB and cache for future queries
            let mut stmt = match conn.prepare("SELECT id FROM symbols WHERE project_id = ?1") {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            let rows: Vec<i64> = stmt
                .query_map([project_id], |r| r.get(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            let ids: HashSet<i64> = rows.into_iter().collect();
            PROJECT_ID_CACHE
                .write()
                .unwrap()
                .insert(project_id, ids.clone());
            ids
        }
    };

    raw.into_iter()
        .filter(|(sym_id, _)| project_ids.contains(sym_id))
        .take(limit)
        .map(|(sym_id, dist)| (sym_id, cosine_distance_to_similarity(dist)))
        .collect()
}

/// Graph proximity from top FTS result → (symbol_id, proximity) pairs.
fn graph_proximity_search(
    conn: &Connection,
    project_id: i64,
    fts_results: &[(i64, f64)],
    limit: usize,
) -> Vec<(i64, f32)> {
    if fts_results.is_empty() {
        return Vec::new();
    }

    let top_id = fts_results[0].0;
    let top_name: String = match conn.query_row(
        "SELECT name FROM symbols WHERE id = ?1",
        rusqlite::params![top_id],
        |row| row.get(0),
    ) {
        Ok(n) => n,
        Err(_) => return Vec::new(),
    };

    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT s.id FROM symbols s
         JOIN edges e ON (e.target = s.name OR e.source = s.name)
         WHERE (e.source = ?1 OR e.target = ?1) AND e.project_id = ?2
         AND s.project_id = ?2 AND s.id != ?3
         LIMIT ?4",
    ) else {
        return Vec::new();
    };

    let ids: Vec<(i64, f32)> = stmt
        .query_map(
            rusqlite::params![top_name, project_id, top_id, limit],
            |row| row.get(0),
        )
        .ok()
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .enumerate()
                .map(|(i, id)| (id, 1.0 / (i as f32 + 2.0)))
                .collect()
        })
        .unwrap_or_default();

    ids
}

/// Fetch multiple symbol rows in a single `WHERE id IN (...)` query.
/// Replaces the N+1 `get_symbol_by_id` pattern.
fn batch_get_symbols(conn: &Connection, ranked: &[(i64, (f32, Vec<String>))]) -> Vec<BrainResult> {
    if ranked.is_empty() {
        return Vec::new();
    }

    // Build placeholder string: ?, ?, ?, ...
    let placeholders = ranked.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT id, name, kind, file, line, signature \
         FROM symbols WHERE id IN ({placeholders})"
    );

    let ids: Vec<i64> = ranked.iter().map(|(id, _)| *id).collect();
    let params: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("batch_get_symbols prepare error: {e}");
            return Vec::new();
        }
    };

    // Build a lookup map: symbol_id → (score, signals)
    let mut score_map: HashMap<i64, (f32, Vec<String>)> = HashMap::with_capacity(ranked.len());
    for (id, (score, signals)) in ranked {
        score_map.insert(*id, (*score, signals.clone()));
    }

    let results: Vec<BrainResult> = stmt
        .query_map(params.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .ok()
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .filter_map(|(id, name, kind, file, line, signature)| {
                    score_map.remove(&id).map(|(score, mut signals)| {
                        signals.sort();
                        signals.dedup();
                        BrainResult {
                            symbol_id: id,
                            name,
                            kind,
                            file,
                            line,
                            signature,
                            score,
                            signals,
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    results
}
