//! Brain Mode — hybrid search combining FTS5 + a code embedding model + graph proximity.
//!
//! RRF fusion (k=60) merges 3 signal sources into ranked results.
//! Pattern adopted from uteke `doc_search_hybrid()`.
//!
//! Vectors are stored one vecq file per project and model (`index::vector`), rebuilt
//! from the project's symbols after every index run that changed them.

use crate::embed::{
    Backend, active_backend, active_dims, active_provider_name, embed_batch_dispatch,
    embed_code_dispatch,
};
use crate::index::symbols::SymbolQuery;
use crate::index::vector::{self, Loaded, ProjectVectors};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

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
    /// The code embedding model is not available (or `brain.embedding: off`):
    /// the vector signal is off, keyword and graph signals answer alone.
    Off,
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
            VectorState::Off => out.push(
                "vector signal is off (the code embedding model is not available): results come \
                 from keyword and graph signals only. Run `cora index` with network access once \
                 to download the model, or see CORA_EMBED_MODEL_DIR"
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

/// Vector file of a project for the active model.
fn project_vector_path(project_id: i64) -> std::path::PathBuf {
    vector::project_file(project_id, active_backend().tag())
}

/// Embed every symbol of a project and replace its vector file.
///
/// The project's whole vector file is rebuilt (embedding is tens of thousands
/// of symbols per second), so it never holds a vector of a symbol that no longer
/// exists, and nothing is shared with other projects. Returns the number of
/// symbols embedded.
pub fn embed_project(conn: &Connection, project_id: i64) -> Result<usize> {
    if active_backend() == Backend::Off {
        // No model, no vectors: drop what an earlier model left so nothing stale is read.
        vector::forget(&project_vector_path(project_id));
        vector::remove_project_files(project_id, None);
        vector::remove_legacy_shared_index();
        return Ok(0);
    }
    let path = project_vector_path(project_id);
    let mut stmt =
        conn.prepare("SELECT id, name, signature FROM symbols WHERE project_id = ?1 ORDER BY id")?;
    let rows: Vec<(i64, String, String)> = stmt
        .query_map(rusqlite::params![project_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        // A project without symbols has no vectors.
        vector::forget(&path);
        let _ = std::fs::remove_file(&path);
        return Ok(0);
    }

    let texts: Vec<String> = rows
        .iter()
        .map(|(_, name, signature)| {
            if signature.is_empty() || signature == name {
                name.clone()
            } else {
                format!("{name} {signature}")
            }
        })
        .collect();

    let t_compute = std::time::Instant::now();
    let vectors = embed_batch_dispatch(&texts);
    let compute_ms = t_compute.elapsed().as_millis();
    let dims = vectors.first().map_or_else(active_dims, Vec::len);
    let count = rows.len();

    let t_write = std::time::Instant::now();
    ProjectVectors::build(dims, rows.iter().map(|r| r.0).zip(vectors))
        .save(&path)
        .context("save the project's vectors")?;
    vector::forget(&path);
    let write_ms = t_write.elapsed().as_millis();

    // The file of a model this project no longer uses, and the former shared
    // index (nothing reads it any more), are dead weight.
    vector::remove_project_files(project_id, Some(active_backend().tag()));
    vector::remove_legacy_shared_index();

    let provider = active_provider_name();
    let tier = active_backend().tier();
    conn.execute(
        "UPDATE projects SET embedding_tier = ?3, embedding_dims = ?1, \
         embedding_provider = ?4, last_embedded_at = datetime('now') WHERE id = ?2",
        // rusqlite 0.41 binds only i64; dims and limits are small.
        rusqlite::params![
            i64::try_from(dims).unwrap_or(i64::MAX),
            project_id,
            tier,
            provider
        ],
    )?;

    tracing::info!(
        "Embedded {count} symbols for project {project_id} (embed {compute_ms} ms, write {write_ms} ms, provider={provider}, dims={dims})",
    );
    Ok(count)
}

/// Hybrid brain search: FTS5 + vector KNN + graph proximity → RRF fusion.
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
    let (vec_hits, vector_state) = vector_search_state(project_id, query, fetch_limit);
    let graph_hits = graph_proximity_search(conn, project_id, &fts_hits, fetch_limit);

    let embedding_tier: Option<String> = conn
        .query_row(
            "SELECT embedding_tier FROM projects WHERE id = ?1",
            [project_id],
            |r| r.get(0),
        )
        .ok();
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

/// True when the project's vector file for the active model is missing, from an
/// older format or built with other dimensions: it must be (re)built even if no
/// source file changed since the last `cora index`, for example on the first run
/// after the model changed.
pub fn project_needs_embedding(project_id: i64) -> bool {
    if active_backend() == Backend::Off {
        return false;
    }
    !matches!(
        vector::load_cached(&project_vector_path(project_id), active_dims()),
        Loaded::Ready(_)
    )
}

/// Meaning-based matches for a multi-word query, to complete a keyword search
/// that found too little: the question is searched with the full hybrid
/// ([`brain_search_report`] in natural-language mode) and symbols the keyword
/// search already returned (`exclude`) are left out. Returns at most `room`
/// results, optionally restricted to a symbol kind and a file path prefix.
pub fn semantic_matches(
    conn: &Connection,
    project_id: i64,
    text: &str,
    exclude: &HashSet<i64>,
    kind: Option<&str>,
    file_prefix: Option<&str>,
    room: usize,
) -> Vec<BrainResult> {
    if room == 0 || text.split_whitespace().count() < 2 {
        return Vec::new();
    }
    let Ok(report) = brain_search_report(conn, project_id, text, 50, true) else {
        return Vec::new();
    };
    report
        .results
        .into_iter()
        .filter(|r| !exclude.contains(&r.symbol_id))
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        .filter(|r| file_prefix.is_none_or(|p| r.file.starts_with(p)))
        .take(room)
        .collect()
}

/// Nearest symbols of the project by meaning → (symbol_id, cosine_similarity)
/// pairs, with how the vector signal fared. Loads only this project's vector
/// file (cached per process, reloaded when the file changes).
fn vector_search_state(
    project_id: i64,
    query: &str,
    limit: usize,
) -> (Vec<(i64, f32)>, VectorState) {
    if active_backend() == Backend::Off {
        return (Vec::new(), VectorState::Off);
    }
    let dims = active_dims();
    let vectors = match vector::load_cached(&project_vector_path(project_id), dims) {
        Loaded::Ready(v) if v.len() > 0 => v,
        Loaded::Ready(_) | Loaded::Missing => return (Vec::new(), VectorState::IndexEmpty),
        Loaded::Unreadable(why) => {
            tracing::warn!(
                "vector file of project {project_id} is unreadable ({why}); run `cora index` to rebuild it"
            );
            return (Vec::new(), VectorState::IndexEmpty);
        }
        Loaded::DimsMismatch { file, wanted } => {
            tracing::warn!(
                "vector file of project {project_id} has {file} dimensions, the model has {wanted}; run `cora index` to rebuild it"
            );
            return (Vec::new(), VectorState::DimsMismatch);
        }
    };

    let vec = embed_code_dispatch(query);
    if vec.len() != vectors.dim() {
        return (Vec::new(), VectorState::DimsMismatch);
    }

    let hits = vectors.search(&vec, limit);
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
            embedding_tier: Some("potion".to_string()),
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
    fn vector_only_results_are_warned_about() {
        let diag = BrainDiagnostics {
            terms: vec![],
            fts_hits: 0,
            vector_hits: 4,
            graph_hits: 0,
            vector: VectorState::Used,
            embedding_tier: Some("potion".to_string()),
        };
        assert!(
            diag.warnings()
                .iter()
                .any(|m| m.contains("every result comes from the vector"))
        );
    }

    /// Without the model there is no vector signal, and the user is told why.
    #[test]
    fn a_missing_model_turns_the_vector_signal_off_with_a_clear_message() {
        let diag = BrainDiagnostics {
            terms: vec![],
            fts_hits: 3,
            vector_hits: 0,
            graph_hits: 0,
            vector: VectorState::Off,
            embedding_tier: None,
        };
        let w = diag.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("vector signal is off") && w[0].contains("CORA_EMBED_MODEL_DIR"));
    }
}
