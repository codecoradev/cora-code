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

/// Why the vector signal did or did not contribute to a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorState {
    /// The index answered with at least one symbol of this project.
    Used,
    /// The index answered, but nothing of this project was near the query.
    NoHits,
    /// No vector index is loaded, or it is empty (`cora index` builds it).
    IndexEmpty,
    /// The index was built with other dimensions than the active embedding.
    DimsMismatch,
}

/// How a brain search was answered: which signals found how much.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BrainDiagnostics {
    /// Search terms the question was reduced to (natural-language mode).
    pub terms: Vec<String>,
    pub fts_hits: usize,
    pub vector_hits: usize,
    pub graph_hits: usize,
    pub vector: VectorState,
    /// Embedding tier the project was indexed with (`static` is text-only).
    pub embedding_tier: Option<String>,
}

impl BrainDiagnostics {
    /// Human-readable warnings about a signal that cannot be trusted.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.vector {
            VectorState::IndexEmpty => out.push(
                "vector index is empty or not loaded: results come from keyword and graph \
                 signals only (run `cora index`)"
                    .to_string(),
            ),
            VectorState::DimsMismatch => out.push(
                "vector index was built with other dimensions than the active embedding: \
                 vector signal skipped (run `cora index --rebuild`)"
                    .to_string(),
            ),
            VectorState::Used if self.embedding_tier.as_deref() == Some("static") => out.push(
                "vector hits come from a static text embedding and may be unrelated to the \
                 question; configure an embedding provider for semantic matches"
                    .to_string(),
            ),
            _ => {}
        }
        if self.fts_hits == 0 && self.vector_hits > 0 {
            out.push("no keyword matches: every result comes from the vector signal".to_string());
        }
        out
    }
}

/// Results of a brain search plus how they were found.
#[derive(Debug, Clone)]
pub struct BrainReport {
    pub results: Vec<BrainResult>,
    pub diagnostics: BrainDiagnostics,
}

/// Words that carry no symbol name in a question.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "in", "is", "it", "its", "of", "on", "or", "that", "the", "this", "to", "was", "what",
    "when", "where", "which", "who", "why", "with",
];

/// Most terms a question is reduced to.
const MAX_TERMS: usize = 8;

/// Reduce a natural-language question to the words that can name a symbol:
/// lowercased, stop words dropped, and each `snake_case` word kept whole and
/// split (`web_search` -> `web_search`, `web`, `search`). Falls back to the
/// whole query when nothing is left.
pub fn question_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let push = |t: String, terms: &mut Vec<String>| {
        if t.len() >= 2 && !STOP_WORDS.contains(&t.as_str()) && !terms.contains(&t) {
            terms.push(t);
        }
    };
    for raw in query.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        let word = raw.trim_matches('_').to_lowercase();
        if word.is_empty() {
            continue;
        }
        push(word.clone(), &mut terms);
        if word.contains('_') {
            for part in word.split('_') {
                push(part.to_string(), &mut terms);
            }
        }
    }
    terms.truncate(MAX_TERMS);
    if terms.is_empty() {
        let whole = query.trim().to_string();
        if !whole.is_empty() {
            terms.push(whole);
        }
    }
    terms
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
fn check_dimension_compat(vi_path: &std::path::Path, expected_dims: usize) -> bool {
    // Attempt to load the index just to check its dimensions.
    // If it fails (empty, corrupt, etc.), we'll create fresh — no warning needed.
    // Returns true when a stale index was removed: the vector index is a
    // single file shared by every project, so its fingerprints are now all
    // stale and callers must clear them.
    let Ok(file) = std::fs::File::open(vi_path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.len() == 0 {
        return false; // Empty file — will create fresh
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
                false
            } else {
                let keys_path = vi_path.with_extension("keys");
                let _ = std::fs::remove_file(&keys_path);
                tracing::info!("  Removed stale vector index — will create fresh on next index");
                true
            }
        }
        Ok(Some(_)) => {
            // Dimensions match — all good
            false
        }
        _ => {
            // Couldn't read dimensions (corrupt, unsupported, etc.)
            // embed_project will handle creation
            false
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
    let dims_stale = if vi_path.exists() {
        check_dimension_compat(&vi_path, active)
    } else {
        false
    };

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

    // ── Self-heal after a global index rebuild (#542, vecq#32) ───────
    // The vector index is one shared file across projects. A vecq reload
    // that had to rebuild it (legacy pre-0.3.0 file, width change, dim
    // change, corruption) or a stale usearch index removed for a dims
    // mismatch leaves every project's vectors gone while their stored
    // fingerprints still say "embedded" — the incremental path would skip
    // those symbols forever. Clear fingerprints for ALL projects: the
    // current project re-embeds in this run; every other project re-embeds
    // on its next `cora index` run that reaches embed_project
    // (cross-project scheduling: #545).
    if dims_stale || (vi.is_dirty() && vi.is_empty()) {
        let cleared = conn.execute("UPDATE symbols SET embed_fingerprint = NULL", [])?;
        tracing::info!(
            cleared,
            "vector index rebuilt from empty — all projects re-embed on their next index run"
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
        // rusqlite 0.41 binds only i64; dims and limits are small.
        rusqlite::params![
            i64::try_from(active).unwrap_or(i64::MAX),
            project_id,
            tier,
            provider
        ],
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
    Ok(brain_search_report(conn, project_id, query, limit, false)?.results)
}

/// `brain_search` that also reports which signals found what.
///
/// With `natural`, the query is a question: it is reduced to its content words
/// and a symbol matching more of them ranks higher. The plain mode requires
/// every word in one symbol name, which finds nothing for a sentence and left
/// only the (noisy) vector signal (#717).
pub fn brain_search_report(
    conn: &Connection,
    project_id: i64,
    query: &str,
    limit: usize,
    natural: bool,
) -> Result<BrainReport> {
    let limit = limit.min(50);
    let fetch_limit = limit * 2;

    let terms = if natural {
        question_terms(query)
    } else {
        Vec::new()
    };
    let fts_hits = if natural {
        fts5_terms_search(conn, project_id, query, &terms, fetch_limit)
    } else {
        fts5_search(conn, project_id, query, fetch_limit)
    };
    let (vec_hits, vector_state) = vector_search_state(conn, project_id, query, fetch_limit);
    let graph_hits = graph_proximity_search(conn, project_id, &fts_hits, fetch_limit);

    let embedding_tier: Option<String> = conn
        .query_row(
            "SELECT embedding_tier FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .ok();
    // A static (text-only) embedding is a weak signal; count it for half.
    let vector_weight = if embedding_tier.as_deref() == Some("static") {
        0.5
    } else {
        1.0
    };

    // ── RRF Fusion ──────────────────────────────────────────────────────
    let mut fused: HashMap<i64, (f32, Vec<String>)> = HashMap::new();

    for (rank, (id, _score)) in fts_hits.iter().enumerate() {
        let rrf = 1.0 / (RRF_K + (rank as f32 + 1.0));
        let entry = fused.entry(*id).or_insert((0.0, Vec::new()));
        entry.0 += rrf;
        entry.1.push("fts".into());
    }

    for (rank, (id, _sim)) in vec_hits.iter().enumerate() {
        let rrf = vector_weight / (RRF_K + (rank as f32 + 1.0));
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
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked.truncate(limit);

    // Batch fetch all symbol rows in a single query (was N+1 individual queries)
    let results = batch_get_symbols(conn, &ranked);

    Ok(BrainReport {
        results,
        diagnostics: BrainDiagnostics {
            terms,
            fts_hits: fts_hits.len(),
            vector_hits: vec_hits.len(),
            graph_hits: graph_hits.len(),
            vector: vector_state,
            embedding_tier,
        },
    })
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

/// FTS5 search for a question: the whole query first (all words in one name),
/// then each content word on its own. A symbol is ranked by how many of the
/// words it matches, then by its best rank for any of them.
fn fts5_terms_search(
    conn: &Connection,
    project_id: i64,
    query: &str,
    terms: &[String],
    limit: usize,
) -> Vec<(i64, f64)> {
    // id -> (terms matched, best position, best score)
    let mut found: HashMap<i64, (usize, usize, f64)> = HashMap::new();
    if terms.len() > 1 {
        for (pos, (id, score)) in fts5_search(conn, project_id, query, limit)
            .into_iter()
            .enumerate()
        {
            found.insert(id, (terms.len(), pos, score));
        }
    }
    for term in terms {
        for (pos, (id, score)) in fts5_search(conn, project_id, term, limit)
            .into_iter()
            .enumerate()
        {
            let e = found.entry(id).or_insert((0, pos, score));
            if e.0 < terms.len() {
                e.0 += 1;
            }
            if pos < e.1 {
                e.1 = pos;
                e.2 = score;
            }
        }
    }
    let mut ranked: Vec<(i64, (usize, usize, f64))> = found.into_iter().collect();
    ranked.sort_by(|a, b| {
        b.1.0
            .cmp(&a.1.0)
            .then_with(|| a.1.1.cmp(&b.1.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(limit)
        .map(|(id, (_, _, score))| (id, score))
        .collect()
}

/// True when the on-disk vector index exists but will be discarded on reload —
/// i.e. vecq backend whose `.vecq` file predates keyed persistence (vecq#32),
/// is unreadable, or was built with different dims. `cora index` uses this to
/// force a re-embed even when no files changed, so the vector signal heals
/// instead of staying empty. Healthy keyed files (vecq-core 0.3.0+) reload
/// as-is and return false.
pub fn vector_index_needs_rebuild() -> bool {
    if crate::index::vector::current_vector_store() != crate::index::vector::VectorStoreKind::Vecq {
        return false;
    }
    let path = vector_index_path().with_extension("vecq");
    path.exists() && crate::index::vector::vecq_file_needs_rebuild(&path, active_dims())
}

/// Return a read guard over the vector index cache, loading the on-disk
/// index once per process if needed (#545). VECTOR_CACHE used to be
/// populated only by `embed_project` (i.e. `cora index`), so a fresh search
/// process (`cora brain`, MCP tools) always saw an empty cache and the
/// vector signal never fired. Load failures degrade gracefully: the cache
/// stays `None` and search falls back to FTS + graph signals.
///
/// `RwLockWriteGuard::downgrade` is still unstable, so this drops the write
/// guard and re-acquires a read lock; a concurrent reader may load twice,
/// which is idempotent (same file, same result).
fn ensure_vector_cache() -> std::sync::RwLockReadGuard<'static, Option<CodeVectorIndex>> {
    {
        let cache = VECTOR_CACHE.read().unwrap();
        if cache.is_some() {
            return cache;
        }
    }

    let mut cache = VECTOR_CACHE.write().unwrap();
    if cache.is_none() {
        let dims = active_dims();
        match CodeVectorIndex::load_or_create(&vector_index_path(), dims) {
            Ok(vi) => *cache = Some(vi),
            Err(e) => tracing::warn!("vector index load failed (search degrades to FTS): {e}"),
        }
    }
    drop(cache);

    VECTOR_CACHE.read().unwrap()
}

/// usearch vector search → (symbol_id, cosine_similarity) pairs, filtered to project.
/// Uses cached vector index and cached project ID set — no disk I/O per query.
fn vector_search_state(
    conn: &Connection,
    project_id: i64,
    query: &str,
    limit: usize,
) -> (Vec<(i64, f32)>, VectorState) {
    // Read-lock the cached vector index — lazy-loads from disk on the first
    // search in this process (#545).
    let cache = ensure_vector_cache();
    let vi = match cache.as_ref() {
        Some(v) if !v.is_empty() => v,
        _ => return (Vec::new(), VectorState::IndexEmpty),
    };

    let vec = embed_code_dispatch(query);

    // Dimension mismatch (e.g. embedding provider switched since the index
    // was built) would panic inside the backend — degrade to other signals.
    if vi.dims() != vec.len() {
        return (Vec::new(), VectorState::DimsMismatch);
    }

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
                Err(_) => return (Vec::new(), VectorState::NoHits),
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

    let hits: Vec<(i64, f32)> = raw
        .into_iter()
        .filter(|(sym_id, _)| project_ids.contains(sym_id))
        .take(limit)
        .map(|(sym_id, dist)| (sym_id, cosine_distance_to_similarity(dist)))
        .collect();
    let state = if hits.is_empty() {
        VectorState::NoHits
    } else {
        VectorState::Used
    };
    (hits, state)
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
            rusqlite::params![
                top_name,
                project_id,
                top_id,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
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

    // `IN (...)` returns rows in table order, not in rank order: restore the
    // fused ranking, best first, ties by symbol id so the order is stable.
    let mut results = results;
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.symbol_id.cmp(&b.symbol_id))
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/brain-fixture").unwrap();
        (conn, pid)
    }

    /// A small project shaped like the web-search question of #717: the
    /// symbols that answer it, and short unrelated ones that matched before.
    fn fixture() -> (Connection, i64) {
        let (conn, pid) = db();
        let src = r#"
pub fn web_search(query: &str) -> String { pick_backend(query) }
pub fn pick_backend(query: &str) -> String { read_env(query) }
pub fn read_env(name: &str) -> String { name.to_string() }
pub fn commit() {}
pub fn is_at_eof() -> bool { true }
pub fn tzif() {}
"#;
        crate::index::index_file(&conn, pid, "src/net.rs", src, "rs").unwrap();
        (conn, pid)
    }

    #[test]
    fn a_question_is_reduced_to_its_content_words() {
        let t = question_terms("how does web_search pick its backend and read env");
        assert_eq!(
            t,
            [
                "web_search",
                "web",
                "search",
                "pick",
                "backend",
                "read",
                "env"
            ]
        );
    }

    /// The fused ranking survives the symbol lookup: results come back best
    /// first, not in table order.
    #[test]
    fn results_are_returned_best_first() {
        let (conn, pid) = fixture();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM symbols WHERE project_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([pid], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        // Ranked against table order: the last symbol scores highest.
        let ranked: Vec<(i64, (f32, Vec<String>))> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, (i as f32, vec!["fts".to_string()])))
            .collect();
        let out = batch_get_symbols(&conn, &ranked);
        let scores: Vec<f32> = out.iter().map(|r| r.score).collect();
        assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
        assert_eq!(out[0].symbol_id, *ids.last().unwrap());
    }

    #[test]
    fn a_query_of_only_stop_words_keeps_the_whole_query() {
        assert_eq!(question_terms("how is it"), ["how is it"]);
        assert!(question_terms("   ").is_empty());
    }

    /// Regression set (#717): the question that returned only unrelated
    /// vector hits now finds the symbols that answer it through keywords.
    #[test]
    fn the_web_search_question_finds_its_answer_by_keyword() {
        let (conn, pid) = fixture();
        let q = "how does web_search pick its backend and read env";
        let report = brain_search_report(&conn, pid, q, 5, true).unwrap();
        // Only the keyword signal is under test: a vector index on disk (the
        // developer's own) may add hits that carry no `fts` signal.
        let got: Vec<&str> = report
            .results
            .iter()
            .filter(|r| r.signals.iter().any(|s| s == "fts"))
            .map(|r| r.name.as_str())
            .collect();
        assert!(got.len() >= 3, "{got:?}");
        // The function the question names and the one it asks about lead;
        // which of the two is first depends on the graph signal.
        assert!(
            got[..2].contains(&"web_search") && got[..2].contains(&"pick_backend"),
            "{got:?}"
        );
        assert!(
            got.contains(&"pick_backend") && got.contains(&"read_env"),
            "{got:?}"
        );
        for noise in ["commit", "is_at_eof", "tzif"] {
            assert!(
                !got.contains(&noise),
                "{noise} must not match by keyword: {got:?}"
            );
        }
        assert!(report.diagnostics.fts_hits >= 3);
    }

    /// The plain mode needs every word in one name, so the same sentence finds
    /// nothing by keyword; that is the failure the natural mode fixes.
    #[test]
    fn plain_mode_finds_nothing_for_a_sentence() {
        let (conn, pid) = fixture();
        let q = "how does web_search pick its backend and read env";
        let plain = brain_search_report(&conn, pid, q, 5, false).unwrap();
        assert_eq!(plain.diagnostics.fts_hits, 0);
    }

    #[test]
    fn an_empty_or_mismatched_vector_index_is_reported() {
        let base = BrainDiagnostics {
            terms: vec![],
            fts_hits: 2,
            vector_hits: 0,
            graph_hits: 0,
            vector: VectorState::IndexEmpty,
            embedding_tier: None,
        };
        assert!(
            base.warnings()
                .iter()
                .any(|w| w.contains("vector index is empty"))
        );
        let mismatch = BrainDiagnostics {
            vector: VectorState::DimsMismatch,
            ..base.clone()
        };
        assert!(
            mismatch
                .warnings()
                .iter()
                .any(|w| w.contains("other dimensions"))
        );
        let healthy = BrainDiagnostics {
            vector: VectorState::Used,
            vector_hits: 3,
            embedding_tier: Some("provider".to_string()),
            ..base
        };
        assert!(healthy.warnings().is_empty());
    }

    /// A search reports its diagnostics whatever state the vector index is in.
    #[test]
    fn a_search_reports_the_signals_that_answered() {
        let (conn, pid) = fixture();
        let report = brain_search_report(&conn, pid, "web_search", 5, true).unwrap();
        assert!(report.diagnostics.fts_hits >= 1);
        assert_eq!(report.diagnostics.terms[0], "web_search");
    }

    #[test]
    fn static_embedding_and_vector_only_results_are_warned_about() {
        let diag = BrainDiagnostics {
            terms: vec![],
            fts_hits: 0,
            vector_hits: 4,
            graph_hits: 0,
            vector: VectorState::Used,
            embedding_tier: Some("static".to_string()),
        };
        let w = diag.warnings();
        assert!(w.iter().any(|m| m.contains("static text embedding")));
        assert!(
            w.iter()
                .any(|m| m.contains("every result comes from the vector"))
        );
    }
}
