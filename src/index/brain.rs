//! Brain Mode — hybrid search combining FTS5 + usearch vectors + graph proximity.
//!
//! RRF fusion (k=60) merges 3 signal sources into ranked results.
//! Pattern adopted from uteke `doc_search_hybrid()`.

use crate::embed::tokens::embed_code;
use crate::index::symbols::SymbolQuery;
use crate::index::vector::{CodeVectorIndex, DEFAULT_DIMS, cosine_distance_to_similarity};
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

/// Embed all symbols for a project into the vector index.
///
/// Reads symbols from SQLite, embeds via static token method,
/// stores in usearch. Updates the in-memory cache.
/// Call after `index_project`.
pub fn embed_project(conn: &Connection, project_id: i64) -> Result<usize> {
    let vi_path = vector_index_path();

    // Acquire write lock — block searches while embedding.
    // This is fine because embedding only happens during `cora index`,
    // never during `cora brain_search`.
    let mut cache = VECTOR_CACHE.write().unwrap();

    // Load or create the vector index (reuses cache if available)
    let vi = if let Some(ref mut cached) = *cache {
        cached
    } else {
        let vi =
            CodeVectorIndex::load_or_create(&vi_path, DEFAULT_DIMS).context("load vector index")?;
        *cache = Some(vi);
        cache.as_mut().unwrap()
    };

    let mut stmt =
        conn.prepare("SELECT id, name, kind, signature FROM symbols WHERE project_id = ?1")?;
    let rows: Vec<(i64, String, String, String)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // ── Parallel embedding computation (Rayon) ─────────────────────────
    // embed_code is pure + CPU-bound. usearch insert is serial.
    let t_compute = std::time::Instant::now();
    let embedded: Vec<(i64, Vec<f32>)> = rows
        .par_iter()
        .map(|(sym_id, name, _kind, signature)| {
            let text = if signature.is_empty() || signature == name {
                name.clone()
            } else {
                format!("{name} {signature}")
            };
            let embedding = embed_code(&text);
            let vec: Vec<f32> = embedding.as_slice().iter().map(|&v| v as f32).collect();
            (*sym_id, vec)
        })
        .collect();
    let compute_ms = t_compute.elapsed().as_millis();

    // ── Serial usearch insert ────────────────────────────────────────
    let t_insert = std::time::Instant::now();
    let mut count = 0;
    let mut new_ids: HashSet<i64> = HashSet::with_capacity(embedded.len());
    for (sym_id, vec) in &embedded {
        vi.insert(*sym_id, vec)
            .context("insert symbol embedding")?;
        new_ids.insert(*sym_id);
        count += 1;
    }
    let insert_ms = t_insert.elapsed().as_millis();

    tracing::debug!(
        "embed_compute={}ms, usearch_insert={}ms, symbols={}",
        compute_ms, insert_ms, count
    );

    if vi.is_dirty() {
        vi.save().context("save vector index")?;
    }

    // Cache project → symbol IDs for fast search-time filtering
    PROJECT_ID_CACHE
        .write()
        .unwrap()
        .insert(project_id, new_ids);

    conn.execute(
        "UPDATE projects SET embedding_tier = 'static', embedding_dims = ?1, \
         last_embedded_at = datetime('now') WHERE id = ?2",
        rusqlite::params![DEFAULT_DIMS, project_id],
    )?;

    tracing::info!("Embedded {count} symbols for project {project_id}");
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

/// usearch vector search → (symbol_id, cosine_similarity) pairs, filtered to project.
/// Uses cached vector index and cached project ID set — no disk I/O per query.
fn vector_search(conn: &Connection, project_id: i64, query: &str, limit: usize) -> Vec<(i64, f32)> {
    // Read-lock the cached vector index — no disk load
    let cache = VECTOR_CACHE.read().unwrap();
    let vi = match cache.as_ref() {
        Some(v) if !v.is_empty() => v,
        _ => return Vec::new(),
    };

    let embedding = embed_code(query);
    let vec: Vec<f32> = embedding.as_slice().iter().map(|&v| v as f32).collect();

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
