//! Evaluation harness: how well do local embeddings find a function from a
//! description of it?
//!
//! usage: eval <data_prefix> <backend> [model_dir] [int8]
//!   backend: bm25 | hashing | nomic | potion
//! Prints one JSON object per line on stdout.

mod embed;

use model2vec_rs::model::StaticModel;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Instant;

struct Doc {
    text: String,
}

/// What a document is made of: the whole function, or only `name signature` as
/// cora indexes symbols today.
fn doc_text(d: &Value, rep: &str) -> String {
    if rep == "sig" {
        format!("{} {}", d["name"].as_str().unwrap(), d["sig"].as_str().unwrap())
    } else {
        d["text"].as_str().unwrap().to_string()
    }
}
struct Query {
    q: String,
    qm: String,
    rel: Vec<usize>,
}

fn rss_mb() -> (f64, f64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    (kb("VmHWM:"), kb("VmRSS:"))
}

fn load(prefix: &str, rep: &str) -> (Vec<Doc>, Vec<Query>) {
    let read = |suffix: &str| -> Vec<Value> {
        std::fs::read_to_string(format!("{prefix}.{suffix}.jsonl"))
            .unwrap_or_else(|e| panic!("{prefix}.{suffix}.jsonl: {e}"))
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let corpus = read("corpus");
    // ids in the files are not dense after filtering: map them to positions
    let mut pos: HashMap<u64, usize> = HashMap::new();
    let mut docs = Vec::new();
    for (i, d) in corpus.iter().enumerate() {
        pos.insert(d["id"].as_u64().unwrap(), i);
        docs.push(Doc { text: doc_text(d, rep) });
    }
    let queries = read("queries")
        .iter()
        .map(|q| Query {
            q: q["q"].as_str().unwrap().to_string(),
            qm: q["qm"].as_str().unwrap().to_string(),
            rel: q["rel"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| pos.get(&r.as_u64().unwrap()).copied())
                .collect(),
        })
        .filter(|q| !q.rel.is_empty())
        .collect();
    (docs, queries)
}

// ── BM25 over identifier-split tokens ───────────────────────────────────────

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if word.is_empty() {
            continue;
        }
        for part in word.split('_') {
            // split camelCase
            let mut cur = String::new();
            let chars: Vec<char> = part.chars().collect();
            for (i, &c) in chars.iter().enumerate() {
                let boundary = i > 0
                    && c.is_uppercase()
                    && (chars[i - 1].is_lowercase()
                        || (i + 1 < chars.len() && chars[i + 1].is_lowercase() && chars[i - 1].is_uppercase()));
                if boundary && !cur.is_empty() {
                    out.push(cur.to_lowercase());
                    cur.clear();
                }
                cur.push(c);
            }
            if !cur.is_empty() {
                out.push(cur.to_lowercase());
            }
        }
    }
    out.retain(|t| t.len() >= 2);
    out
}

struct Bm25 {
    postings: HashMap<String, Vec<(usize, f32)>>,
    idf: HashMap<String, f32>,
    len: Vec<f32>,
    avg: f32,
}

impl Bm25 {
    fn new(docs: &[Doc]) -> Self {
        let mut postings: HashMap<String, Vec<(usize, f32)>> = HashMap::new();
        let mut len = Vec::with_capacity(docs.len());
        for (i, d) in docs.iter().enumerate() {
            let toks = tokens(&d.text);
            len.push(toks.len() as f32);
            let mut tf: HashMap<&str, f32> = HashMap::new();
            for t in &toks {
                *tf.entry(t.as_str()).or_default() += 1.0;
            }
            for (t, f) in tf {
                postings.entry(t.to_string()).or_default().push((i, f));
            }
        }
        let n = docs.len() as f32;
        let idf = postings
            .iter()
            .map(|(t, p)| (t.clone(), ((n - p.len() as f32 + 0.5) / (p.len() as f32 + 0.5) + 1.0).ln()))
            .collect();
        let avg = len.iter().sum::<f32>() / n;
        Self { postings, idf, len, avg }
    }

    fn scores(&self, query: &str) -> Vec<f32> {
        let mut s = vec![0.0f32; self.len.len()];
        let (k1, b) = (1.2f32, 0.75f32);
        let mut seen = std::collections::HashSet::new();
        for t in tokens(query) {
            if !seen.insert(t.clone()) {
                continue;
            }
            if let (Some(p), Some(idf)) = (self.postings.get(&t), self.idf.get(&t)) {
                for &(d, f) in p {
                    let norm = f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * self.len[d] / self.avg));
                    s[d] += idf * norm;
                }
            }
        }
        s
    }
}

// ── Dense backends ──────────────────────────────────────────────────────────

enum Dense {
    Hashing,
    Nomic,
    Potion(Box<StaticModel>),
}

impl Dense {
    fn dim(&self) -> usize {
        match self {
            Dense::Hashing => embed::tokens::EMBEDDING_DIM,
            Dense::Nomic => embed::token_vocab::PRETRAINED_DIM,
            Dense::Potion(m) => m.dim(),
        }
    }
    fn embed_all(&self, texts: &[String]) -> Vec<f32> {
        match self {
            Dense::Hashing => texts
                .iter()
                .flat_map(|t| embed::tokens::embed_code(t).as_slice().iter().map(|v| *v as f32).collect::<Vec<_>>())
                .collect(),
            Dense::Nomic => texts.iter().flat_map(|t| embed::token_vocab::embed_code_pretrained(t)).collect(),
            Dense::Potion(m) => m.encode_flat(texts),
        }
    }
}

fn normalize(v: &mut [f32], dim: usize) {
    for row in v.chunks_exact_mut(dim) {
        let n = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            row.iter_mut().for_each(|x| *x /= n);
        }
    }
}

fn dense_scores(docs: &[f32], dim: usize, q: &[f32]) -> Vec<f32> {
    docs.chunks_exact(dim).map(|d| d.iter().zip(q).map(|(a, b)| a * b).sum()).collect()
}

// ── Ranking and metrics ─────────────────────────────────────────────────────

fn order(scores: &[f32]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..scores.len()).collect();
    idx.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
    idx
}

/// 1-based rank of the best-placed relevant document in `ranked`, if any.
fn first_rank(ranked: &[usize], rel: &[usize]) -> Option<usize> {
    ranked.iter().position(|d| rel.contains(d)).map(|p| p + 1)
}

fn ndcg10(ranked: &[usize], rel: &[usize]) -> f64 {
    let dcg: f64 = ranked
        .iter()
        .take(10)
        .enumerate()
        .filter(|(_, d)| rel.contains(d))
        .map(|(i, _)| 1.0 / ((i as f64) + 2.0).log2())
        .sum();
    let ideal: f64 = (0..rel.len().min(10)).map(|i| 1.0 / ((i as f64) + 2.0).log2()).sum();
    if ideal > 0.0 { dcg / ideal } else { 0.0 }
}

/// Reciprocal rank fusion (k = 60) of weighted ranked lists, each cut at `cut`.
fn rrf(lists: &[(&[usize], f64)], cut: usize) -> Vec<usize> {
    let mut score: HashMap<usize, f64> = HashMap::new();
    for (list, w) in lists {
        for (r, d) in list.iter().take(cut).enumerate() {
            *score.entry(*d).or_default() += w / (60.0 + r as f64 + 1.0);
        }
    }
    let mut v: Vec<(usize, f64)> = score.into_iter().collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    v.into_iter().map(|x| x.0).collect()
}

struct Tally {
    rr: Vec<f64>,
    r1: f64,
    r5: f64,
    r10: f64,
    ndcg: f64,
}

impl Tally {
    fn new() -> Self {
        Self { rr: Vec::new(), r1: 0.0, r5: 0.0, r10: 0.0, ndcg: 0.0 }
    }
    fn add(&mut self, ranked: &[usize], rel: &[usize]) {
        let r = first_rank(ranked, rel);
        self.rr.push(match r {
            Some(r) if r <= 10 => 1.0 / r as f64,
            _ => 0.0,
        });
        self.r1 += f64::from(r == Some(1));
        self.r5 += f64::from(r.is_some_and(|r| r <= 5));
        self.r10 += f64::from(r.is_some_and(|r| r <= 10));
        self.ndcg += ndcg10(ranked, rel);
    }
    fn json(&self, method: &str, form: &str) -> Value {
        let n = self.rr.len() as f64;
        json!({"method": method, "form": form, "n": self.rr.len(),
               "mrr10": self.rr.iter().sum::<f64>() / n, "r1": self.r1 / n, "r5": self.r5 / n,
               "r10": self.r10 / n, "ndcg10": self.ndcg / n, "rr": self.rr})
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let prefix = &a[1];
    let backend = a[2].as_str();
    let int8 = a.iter().any(|x| x == "int8");
    let rep = std::env::var("DOCREP").unwrap_or_else(|_| "body".to_string());
    let label = std::env::var("LABEL").unwrap_or_else(|_| backend.to_string());
    let (docs, queries) = load(prefix, &rep);
    let texts: Vec<String> = docs.iter().map(|d| d.text.clone()).collect();
    let (_, rss_data) = rss_mb();
    let mut out: Vec<Value> = Vec::new();
    let forms = [("q", 0usize), ("qm", 1usize)];

    let bm25 = Bm25::new(&docs);
    let bm_orders: Vec<Vec<Vec<usize>>> = forms
        .iter()
        .map(|(_, f)| queries.iter().map(|q| order(&bm25.scores(if *f == 0 { &q.q } else { &q.qm }))).collect())
        .collect();

    let mut info = json!({"backend": label, "rep": rep, "int8": int8, "docs": docs.len(), "queries": queries.len()});

    if backend == "bm25" {
        for (fi, (form, _)) in forms.iter().enumerate() {
            let mut t = Tally::new();
            for (qi, q) in queries.iter().enumerate() {
                t.add(&bm_orders[fi][qi], &q.rel);
            }
            out.push(t.json("bm25", form));
        }
        out.push(info);
    } else {
        let t = Instant::now();
        let dense = match backend {
            "hashing" => Dense::Hashing,
            "nomic" => Dense::Nomic,
            "potion" => {
                let mut m = StaticModel::from_pretrained(&a[3], None, None, None).expect("model");
                if int8 {
                    m = m.quantize_int8();
                }
                Dense::Potion(Box::new(m))
            }
            other => panic!("unknown backend {other}"),
        };
        // touch the lazy vocabularies so the load is counted here
        let _ = dense.embed_all(&["warm up".to_string()]);
        let load_ms = t.elapsed().as_secs_f64() * 1e3;
        let (_, rss_loaded) = rss_mb();
        let dim = dense.dim();

        let t = Instant::now();
        let mut dv = dense.embed_all(&texts);
        let embed_ms = t.elapsed().as_secs_f64() * 1e3;
        normalize(&mut dv, dim);
        let (peak, rss_after) = rss_mb();

        let t = Instant::now();
        let mut qv: Vec<Vec<f32>> = Vec::new();
        for (_, f) in &forms {
            let qs: Vec<String> = queries.iter().map(|q| if *f == 0 { q.q.clone() } else { q.qm.clone() }).collect();
            let mut e = dense.embed_all(&qs);
            normalize(&mut e, dim);
            qv.push(e);
        }
        let query_ms = t.elapsed().as_secs_f64() * 1e3 / (2 * queries.len()) as f64;

        for (fi, (form, _)) in forms.iter().enumerate() {
            let mut dense_t = Tally::new();
            let weights = [0.25f64, 0.5, 1.0, 2.0];
            let mut hybrids: Vec<Tally> = weights.iter().map(|_| Tally::new()).collect();
            for (qi, q) in queries.iter().enumerate() {
                let s = dense_scores(&dv, dim, &qv[fi][qi * dim..(qi + 1) * dim]);
                let ranked = order(&s);
                dense_t.add(&ranked, &q.rel);
                for (wi, w) in weights.iter().enumerate() {
                    let fused = rrf(&[(&bm_orders[fi][qi], 1.0), (&ranked, *w)], 100);
                    hybrids[wi].add(&fused, &q.rel);
                }
            }
            out.push(dense_t.json(&label, form));
            for (wi, w) in weights.iter().enumerate() {
                out.push(hybrids[wi].json(&format!("bm25+{label}@{w}"), form));
            }
        }
        info["dim"] = json!(dim);
        info["load_ms"] = json!(load_ms);
        info["embed_corpus_ms"] = json!(embed_ms);
        info["docs_per_s"] = json!(docs.len() as f64 / (embed_ms / 1e3));
        info["query_ms"] = json!(query_ms);
        info["rss_data_mb"] = json!(rss_data);
        info["rss_loaded_mb"] = json!(rss_loaded);
        info["rss_after_mb"] = json!(rss_after);
        info["peak_rss_mb"] = json!(peak);
        info["index_vectors_mb"] = json!((docs.len() * dim * 4) as f64 / 1e6);
        out.push(info);
    }
    for v in out {
        println!("{v}");
    }
}
