//! Spike: embed this repository's functions with potion-code-16M-v2 and report
//! load time, throughput and memory, plus a few retrieval samples.
//!
//! usage: potion-code-spike <model_dir> <source_dir> [int8]

use anyhow::{Context, Result};
use model2vec_rs::model::StaticModel;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Peak and current resident memory of this process, in MB (Linux).
fn rss_mb() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| {
        status
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    (kb("VmHWM:"), kb("VmRSS:"))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Split a Rust file into function-sized chunks (a rough cut, enough for a spike).
fn chunks(path: &Path, text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let flush = |cur: &mut Vec<&str>, out: &mut Vec<(String, String)>| {
        if !cur.is_empty() {
            let body: String = cur.join("\n").chars().take(1500).collect();
            let head = cur[0].trim().to_string();
            out.push((format!("{} :: {}", path.display(), head), body));
            cur.clear();
        }
    };
    for line in text.lines() {
        let t = line.trim_start();
        let is_fn = t.starts_with("fn ")
            || t.starts_with("pub fn ")
            || t.starts_with("pub(crate) fn ")
            || t.starts_with("async fn ")
            || t.starts_with("pub async fn ");
        if is_fn {
            flush(&mut cur, &mut out);
        }
        if is_fn || !cur.is_empty() {
            cur.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb).max(1e-9)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model_dir = args.get(1).context("model dir")?;
    let src_dir = args.get(2).context("source dir")?;
    let int8 = args.get(3).is_some_and(|a| a == "int8");

    let (_, rss0) = rss_mb();
    let t = Instant::now();
    let mut model = StaticModel::from_pretrained(model_dir, None, None, None)?;
    if int8 {
        model = model.quantize_int8();
    }
    let load = t.elapsed();
    let (_, rss_load) = rss_mb();
    println!(
        "model: dim={} int8={int8} load={:.0} ms  rss {:.0} -> {:.0} MB",
        model.dim(),
        load.as_secs_f64() * 1e3,
        rss0,
        rss_load
    );

    let mut files = Vec::new();
    walk(Path::new(src_dir), &mut files);
    let mut corpus: Vec<(String, String)> = Vec::new();
    for f in &files {
        if let Ok(text) = std::fs::read_to_string(f) {
            corpus.extend(chunks(f, &text));
        }
    }
    let total_chars: usize = corpus.iter().map(|c| c.1.len()).sum();
    println!("corpus: {} files, {} function chunks, {:.1} MB of text", files.len(), corpus.len(), total_chars as f64 / 1e6);

    let texts: Vec<String> = corpus.iter().map(|c| c.1.clone()).collect();
    let t = Instant::now();
    let flat = model.encode_flat(&texts);
    let enc = t.elapsed();
    let dim = model.dim();
    println!(
        "encode: {:.0} ms total, {:.0} chunks/s, {:.2} MB/s of text",
        enc.as_secs_f64() * 1e3,
        texts.len() as f64 / enc.as_secs_f64(),
        total_chars as f64 / 1e6 / enc.as_secs_f64()
    );
    let (peak, now) = rss_mb();
    println!("memory after encode: rss {:.0} MB, peak {:.0} MB; vectors {:.1} MB", now, peak, flat.len() as f64 * 4.0 / 1e6);

    let queries = [
        "how does the review store prune old findings",
        "parse the severity word the model returned",
        "split a diff into chunks by size budget",
        "resolve Type::method calls in the call graph",
        "find dead code that nothing calls",
        "which files are affected by a change",
        "retry a failed request in smaller pieces",
        "load the configuration from a yaml file",
    ];
    let t = Instant::now();
    let q_emb = model.encode(&queries.iter().map(|q| q.to_string()).collect::<Vec<_>>());
    println!("queries: {} encoded in {:.2} ms", queries.len(), t.elapsed().as_secs_f64() * 1e3);
    for (q, qv) in queries.iter().zip(&q_emb) {
        let mut scored: Vec<(f32, usize)> = flat
            .chunks_exact(dim)
            .enumerate()
            .map(|(i, v)| (cosine(qv, v), i))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        println!("\nQ: {q}");
        for (s, i) in scored.iter().take(3) {
            let label = &corpus[*i].0;
            let short: String = label.chars().rev().take(90).collect::<Vec<_>>().into_iter().rev().collect();
            println!("  {s:.3}  ...{short}");
        }
    }
    let (peak, _) = rss_mb();
    println!("\npeak rss {peak:.0} MB");
    Ok(())
}
