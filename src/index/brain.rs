//! Brain Mode — hybrid search combining FTS5 + usearch vectors + graph proximity.
//!
//! RRF fusion (k=60) merges 3 signal sources into ranked results.
//! Pattern adopted from uteke `doc_search_hybrid()`.

use crate::embed::tokens::embed_code;
use crate::index::symbols::SymbolQuery;
use crate::index::vector::{CodeVectorIndex, DEFAULT_DIMS, cosine_distance_to_similarity};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;

/// RRF constant (standard value from Cormack et al. 2009).
const RRF_K: f32 = 60.0;

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
/// stores in usearch. Call after `index_project`.
pub fn embed_project(conn: &Connection, project_id: i64) -> Result<usize> {
    let vi_path = vector_index_path();
    let mut vi =
        CodeVectorIndex::load_or_create(&vi_path, DEFAULT_DIMS).context("load vector index")?;

    let mut stmt =
        conn.prepare("SELECT id, name, kind, signature FROM symbols WHERE project_id = ?1")?;
    let rows: Vec<(i64, String, String, String)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for (sym_id, name, _kind, signature) in &rows {
        let text = if signature.is_empty() || signature == name {
            name.clone()
        } else {
            format!("{name} {signature}")
        };

        let embedding = embed_code(&text);
        let vec: Vec<f32> = embedding.as_slice().iter().map(|&v| v as f32).collect();

        vi.insert(*sym_id, &vec)
            .context("insert symbol embedding")?;
        count += 1;
    }

    if vi.is_dirty() {
        vi.save().context("save vector index")?;
    }

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

    let results: Vec<BrainResult> = ranked
        .into_iter()
        .filter_map(|(id, (score, mut signals))| {
            get_symbol_by_id(conn, id).ok().map(|sym| {
                signals.sort();
                signals.dedup();
                BrainResult {
                    symbol_id: id,
                    name: sym.0,
                    kind: sym.1,
                    file: sym.2,
                    line: sym.3,
                    signature: sym.4,
                    score,
                    signals,
                }
            })
        })
        .collect();

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
/// Over-fetches from global vector index then filters by project_id via DB lookup.
fn vector_search(conn: &Connection, project_id: i64, query: &str, limit: usize) -> Vec<(i64, f32)> {
    let vi_path = vector_index_path();
    if !vi_path.exists() {
        return Vec::new();
    }
    let vi = match CodeVectorIndex::load_or_create(&vi_path, DEFAULT_DIMS) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("vector index load error: {e}");
            return Vec::new();
        }
    };

    if vi.is_empty() {
        return Vec::new();
    }

    let embedding = embed_code(query);
    let vec: Vec<f32> = embedding.as_slice().iter().map(|&v| v as f32).collect();

    // Over-fetch to compensate for post-filter by project_id.
    let over_fetch = (limit * 5).max(50);
    let raw = vi.search(&vec, over_fetch);

    // Build a set of symbol IDs belonging to this project.
    let project_ids: std::collections::HashSet<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM symbols WHERE project_id = ?1")
            .unwrap();
        let rows: Vec<i64> = stmt
            .query_map([project_id], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows.into_iter().collect()
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

/// Fetch symbol row by ID → (name, kind, file, line, signature).
fn get_symbol_by_id(conn: &Connection, id: i64) -> Result<(String, String, String, i64, String)> {
    conn.query_row(
        "SELECT name, kind, file, line, signature FROM symbols WHERE id = ?1",
        rusqlite::params![id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .map_err(Into::into)
}
