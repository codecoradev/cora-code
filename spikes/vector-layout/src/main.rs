//! Vector layout spike.
//!
//! usage: layout <projects_dir> <model_dir> <phase> [arg]
//!   phase: quality | perf | mem <global-usearch|global-vecq|project-usearch|project-vecq>

use anyhow::Result;
use model2vec_rs::model::StaticModel;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
use vecq_core::VecqIndex;

const DIM: usize = 256;
/// What cora does: ask for `limit * 2` results, over-fetch `max(5 * that, 50)`.
/// `LIMIT` (env, default 10) is cora's `--limit`: 10 gives 20 and 100, the CLI
/// default of 20 gives 40 and 200.
fn limit() -> usize {
    static L: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var("LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(10))
}
fn fetch() -> usize {
    limit() * 2
}
fn cora_overfetch() -> usize {
    (fetch() * 5).max(50)
}

struct Project {
    name: String,
    /// First global id of this project's functions.
    offset: usize,
    n: usize,
    /// (query vector, local id of the function the query describes)
    queries: Vec<(Vec<f32>, usize)>,
}

struct Data {
    projects: Vec<Project>,
    /// All function vectors, project after project.
    vecs: Vec<f32>,
    /// Project index of every global id.
    proj_of: Vec<u32>,
}

impl Data {
    fn vec(&self, gid: usize) -> &[f32] {
        &self.vecs[gid * DIM..(gid + 1) * DIM]
    }
    fn total(&self) -> usize {
        self.proj_of.len()
    }
}

fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

fn load_data(dir: &Path, model: &StaticModel) -> Result<Data> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    files.sort();
    let mut projects = Vec::new();
    let mut vecs: Vec<f32> = Vec::new();
    let mut proj_of: Vec<u32> = Vec::new();
    for (pi, f) in files.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(f)?)?;
        let docs: Vec<String> = v["docs"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect();
        let qs = v["queries"].as_array().unwrap();
        let qtexts: Vec<String> = qs.iter().map(|q| q["q"].as_str().unwrap().to_string()).collect();
        let qrel: Vec<usize> = qs.iter().map(|q| q["rel"].as_u64().unwrap() as usize).collect();
        let offset = proj_of.len();
        vecs.extend(model.encode_flat(&docs));
        proj_of.extend(std::iter::repeat_n(pi as u32, docs.len()));
        let qv = model.encode_flat(&qtexts);
        let queries = qv.chunks_exact(DIM).map(|c| c.to_vec()).zip(qrel).collect();
        projects.push(Project { name: v["name"].as_str().unwrap().to_string(), offset, n: docs.len(), queries });
    }
    Ok(Data { projects, vecs, proj_of })
}

fn usearch_index(cap: usize) -> Index {
    let idx = Index::new(&IndexOptions {
        dimensions: DIM,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        ..Default::default()
    })
    .unwrap();
    idx.reserve(cap.max(16)).unwrap();
    idx
}

fn vecq_index() -> VecqIndex {
    VecqIndex::with_residual(DIM, 42)
}

// ── layouts ────────────────────────────────────────────────────────────────

enum Layout {
    GlobalUsearch(Index),
    GlobalVecq(VecqIndex),
    ProjectUsearch(HashMap<usize, Index>),
    ProjectVecq(HashMap<usize, VecqIndex>),
}

/// How a global index is asked for one project's neighbours.
#[derive(Clone, Copy, PartialEq)]
enum Ask {
    /// cora today: fetch 100, keep the project's, take 20.
    Cora,
    /// fetch 1000.
    Wide,
    /// fetch more until 20 of the project are found (or everything was read).
    Adaptive,
    /// usearch filtered search on the project.
    Filtered,
}

fn global_ids_to_local(data: &Data, p: usize, gids: impl Iterator<Item = u64>) -> Vec<usize> {
    gids.filter(|g| data.proj_of[*g as usize] as usize == p)
        .map(|g| g as usize - data.projects[p].offset)
        .take(fetch())
        .collect()
}

fn search(layout: &Layout, data: &Data, p: usize, q: &[f32], ask: Ask) -> Vec<usize> {
    match layout {
        Layout::GlobalUsearch(idx) => match ask {
            Ask::Filtered => {
                let m = idx.filtered_search(q, fetch(), |k| data.proj_of[k as usize] as usize == p).unwrap();
                m.keys.iter().map(|k| *k as usize - data.projects[p].offset).collect()
            }
            Ask::Cora | Ask::Wide => {
                let over = if ask == Ask::Cora { cora_overfetch() } else { 1000 };
                let m = idx.search(q, over.min(idx.size().max(1))).unwrap();
                global_ids_to_local(data, p, m.keys.iter().copied())
            }
            Ask::Adaptive => {
                let mut over = cora_overfetch();
                loop {
                    let m = idx.search(q, over.min(idx.size().max(1))).unwrap();
                    let hits = global_ids_to_local(data, p, m.keys.iter().copied());
                    if hits.len() >= fetch() || over >= idx.size() {
                        return hits;
                    }
                    over *= 2;
                }
            }
        },
        Layout::GlobalVecq(idx) => {
            let mut over = if ask == Ask::Wide { 1000 } else { cora_overfetch() };
            loop {
                let r = idx.search_keyed(q, over);
                let hits = global_ids_to_local(data, p, r.iter().map(|(k, _)| *k));
                if ask != Ask::Adaptive || hits.len() >= fetch() || over >= idx.len() {
                    return hits;
                }
                over *= 2;
            }
        }
        Layout::ProjectUsearch(m) => {
            let idx = &m[&p];
            idx.search(q, fetch().min(idx.size().max(1))).unwrap().keys.iter().map(|k| *k as usize).collect()
        }
        Layout::ProjectVecq(m) => m[&p].search_keyed(q, fetch()).iter().map(|(k, _)| *k as usize).collect(),
    }
}

fn exact(data: &Data, p: usize, q: &[f32]) -> Vec<usize> {
    let pr = &data.projects[p];
    let mut s: Vec<(f32, usize)> = (0..pr.n)
        .map(|i| (data.vec(pr.offset + i).iter().zip(q).map(|(a, b)| a * b).sum::<f32>(), i))
        .collect();
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
    s.into_iter().take(fetch()).map(|x| x.1).collect()
}

fn build_global_usearch(data: &Data, set: &[usize]) -> Index {
    let total: usize = set.iter().map(|p| data.projects[*p].n).sum();
    let idx = usearch_index(total);
    for p in set {
        let pr = &data.projects[*p];
        for i in 0..pr.n {
            idx.add((pr.offset + i) as u64, data.vec(pr.offset + i)).unwrap();
        }
    }
    idx
}

fn build_global_vecq(data: &Data, set: &[usize]) -> VecqIndex {
    let mut idx = vecq_index();
    for p in set {
        let pr = &data.projects[*p];
        for i in 0..pr.n {
            idx.add_keyed((pr.offset + i) as u64, data.vec(pr.offset + i));
        }
    }
    idx
}

fn build_project_usearch(data: &Data, p: usize) -> Index {
    let pr = &data.projects[p];
    let idx = usearch_index(pr.n);
    for i in 0..pr.n {
        idx.add(i as u64, data.vec(pr.offset + i)).unwrap();
    }
    idx
}

fn build_project_vecq(data: &Data, p: usize) -> VecqIndex {
    let pr = &data.projects[p];
    let mut idx = vecq_index();
    for i in 0..pr.n {
        idx.add_keyed(i as u64, data.vec(pr.offset + i));
    }
    idx
}

// ── metrics ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Score {
    n: usize,
    rr: f64,
    r10: f64,
    returned: f64,
    empty: f64,
}

impl Score {
    fn add(&mut self, ranked: &[usize], rel: usize) {
        self.n += 1;
        let pos = ranked.iter().position(|d| *d == rel);
        if let Some(r) = pos.filter(|r| *r < 10) {
            self.rr += 1.0 / (r + 1) as f64;
            self.r10 += 1.0;
        }
        self.returned += ranked.len() as f64;
        self.empty += f64::from(ranked.is_empty());
    }
    fn line(&self) -> String {
        let n = self.n as f64;
        format!(
            "MRR@10 {:.3}  R@10 {:.3}  returned {:>4.1}/{}  empty {:>5.1}%",
            self.rr / n,
            self.r10 / n,
            self.returned / n,
            fetch(),
            100.0 * self.empty / n
        )
    }
}

fn targets(data: &Data) -> Vec<usize> {
    let n = data.projects.len();
    (0..12).map(|i| i * n / 12).collect()
}

fn quality(data: &Data) {
    let tg = targets(data);
    let others: Vec<usize> = (0..data.projects.len()).filter(|p| !tg.contains(p)).collect();
    println!("{} projects, {} functions; targets: {} projects, {} queries\n",
        data.projects.len(), data.total(), tg.len(), tg.iter().map(|p| data.projects[*p].queries.len()).sum::<usize>());

    // per-project layouts and the exact ceiling do not depend on N
    let mut pu = HashMap::new();
    let mut pv = HashMap::new();
    for p in &tg {
        pu.insert(*p, build_project_usearch(data, *p));
        pv.insert(*p, build_project_vecq(data, *p));
    }
    let pu = Layout::ProjectUsearch(pu);
    let pv = Layout::ProjectVecq(pv);

    for &nproj in &[12usize, 40, 176.min(data.projects.len())] {
        let set: Vec<usize> = if nproj <= tg.len() {
            tg[..nproj].to_vec()
        } else {
            let mut s = tg.clone();
            s.extend(others.iter().take(nproj - tg.len()));
            s
        };
        let eval_targets: Vec<usize> = set.iter().copied().filter(|p| tg.contains(p)).collect();
        let total: usize = set.iter().map(|p| data.projects[*p].n).sum();
        let gu = Layout::GlobalUsearch(build_global_usearch(data, &set));
        let gv = Layout::GlobalVecq(build_global_vecq(data, &set));

        let runs: Vec<(&str, &Layout, Ask)> = vec![
            ("global usearch, cora over-fetch", &gu, Ask::Cora),
            ("global usearch, fetch x10", &gu, Ask::Wide),
            ("global usearch, adaptive fetch", &gu, Ask::Adaptive),
            ("global usearch, filtered search", &gu, Ask::Filtered),
            ("global vecq, cora over-fetch", &gv, Ask::Cora),
            ("global vecq, adaptive fetch", &gv, Ask::Adaptive),
            ("per-project usearch", &pu, Ask::Cora),
            ("per-project vecq", &pv, Ask::Cora),
        ];
        println!("== {nproj} projects in the index ({total} vectors), {} target projects", eval_targets.len());
        let mut ex = Score::default();
        for p in &eval_targets {
            for (q, rel) in &data.projects[*p].queries {
                ex.add(&exact(data, *p, q), *rel);
            }
        }
        println!("  {:<34} {}", "exact (brute force, ceiling)", ex.line());
        for (name, layout, ask) in runs {
            if matches!(layout, Layout::ProjectUsearch(_) | Layout::ProjectVecq(_)) && nproj != 12 && nproj != 1 {
                // identical for every N: print once at N = 12
                continue;
            }
            let mut s = Score::default();
            for p in &eval_targets {
                for (q, rel) in &data.projects[*p].queries {
                    s.add(&search(layout, data, *p, q, ask), *rel);
                }
            }
            println!("  {:<34} {}", name, s.line());
        }
        println!();
    }
}

/// A monorepo: the first `nproj` projects are ONE project, so searches are not
/// filtered. Per-project storage cannot shrink this index; does quantized
/// brute force (vecq) still hold up against HNSW (usearch) at that size?
fn mono(data: &Data) {
    let tg = targets(data);
    let others: Vec<usize> = (0..data.projects.len()).filter(|p| !tg.contains(p)).collect();
    println!("one big project; {} results asked, queries from the {} target crates\n", fetch(), tg.len());
    for &nproj in &[12usize, 40, 100, data.projects.len()] {
        let mut set = tg.clone();
        set.extend(others.iter().take(nproj - tg.len()));
        let total: usize = set.iter().map(|p| data.projects[*p].n).sum();
        let (u, tu) = time(|| build_global_usearch(data, &set));
        let (v, tv) = time(|| build_global_vecq(data, &set));
        let mut rows: Vec<(&str, Score, Vec<f64>)> = vec![
            ("exact", Score::default(), vec![]),
            ("usearch", Score::default(), vec![]),
            ("vecq residual", Score::default(), vec![]),
        ];
        for p in &tg {
            let off = data.projects[*p].offset;
            for (q, rel) in &data.projects[*p].queries {
                let gid = off + rel;
                // exact over everything
                let mut sc: Vec<(f32, usize)> = set
                    .iter()
                    .flat_map(|sp| {
                        let pr = &data.projects[*sp];
                        (0..pr.n).map(move |i| pr.offset + i)
                    })
                    .map(|g| (data.vec(g).iter().zip(q).map(|(a, b)| a * b).sum::<f32>(), g))
                    .collect();
                sc.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
                let ex: Vec<usize> = sc.into_iter().take(fetch()).map(|x| x.1).collect();
                rows[0].1.add(&ex, gid);
                let t = Instant::now();
                let ru: Vec<usize> = u.search(q, fetch()).unwrap().keys.iter().map(|k| *k as usize).collect();
                rows[1].2.push(t.elapsed().as_secs_f64() * 1e6);
                rows[1].1.add(&ru, gid);
                let t = Instant::now();
                let rv: Vec<usize> = v.search_keyed(q, fetch()).iter().map(|(k, _)| *k as usize).collect();
                rows[2].2.push(t.elapsed().as_secs_f64() * 1e6);
                rows[2].1.add(&rv, gid);
            }
        }
        println!("== {total} vectors in one project (build: usearch {tu:.0} ms, vecq {tv:.0} ms)");
        for (name, s, mut lat) in rows {
            if lat.is_empty() {
                println!("  {:<16} {}", name, s.line());
            } else {
                println!("  {:<16} {}   latency {:.0} / {:.0} us (median / p95)", name, s.line(), pct(&mut lat, 0.5), pct(&mut lat, 0.95));
            }
        }
        println!();
    }
}

fn time<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64() * 1e3)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p) as usize]
}

fn perf(data: &Data, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let all: Vec<usize> = (0..data.projects.len()).collect();
    println!("{} projects, {} vectors, {} dims\n", all.len(), data.total(), DIM);

    // build
    let (gu, t_gu) = time(|| build_global_usearch(data, &all));
    let (gv, t_gv) = time(|| build_global_vecq(data, &all));
    let (pu, t_pu) = time(|| all.iter().map(|p| (*p, build_project_usearch(data, *p))).collect::<HashMap<_, _>>());
    let (pv, t_pv) = time(|| all.iter().map(|p| (*p, build_project_vecq(data, *p))).collect::<HashMap<_, _>>());
    println!("build (all vectors, 1 thread):");
    println!("  global usearch       {t_gu:>9.0} ms");
    println!("  global vecq          {t_gv:>9.0} ms");
    println!("  per-project usearch  {t_pu:>9.0} ms (sum of {} indexes)", all.len());
    println!("  per-project vecq     {t_pv:>9.0} ms\n");

    // save
    let gu_path = dir.join("global.usearch");
    let (_, s_gu) = time(|| gu.save(gu_path.to_str().unwrap()).unwrap());
    let gv_path = dir.join("global.vecq");
    let (_, s_gv) = time(|| std::fs::write(&gv_path, gv.to_bytes()).unwrap());
    let mut pu_bytes = 0u64;
    let (_, s_pu) = time(|| {
        for (p, idx) in &pu {
            let path = dir.join(format!("p{p}.usearch"));
            idx.save(path.to_str().unwrap()).unwrap();
            pu_bytes += std::fs::metadata(&path).unwrap().len();
        }
    });
    let mut pv_bytes = 0u64;
    let (_, s_pv) = time(|| {
        for (p, idx) in &pv {
            let path = dir.join(format!("p{p}.vecq"));
            std::fs::write(&path, idx.to_bytes()).unwrap();
            pv_bytes += std::fs::metadata(&path).unwrap().len();
        }
    });
    let mb = |b: u64| b as f64 / 1e6;
    println!("save + size:");
    println!("  global usearch       {s_gu:>8.0} ms  {:>7.1} MB", mb(std::fs::metadata(&gu_path)?.len()));
    println!("  global vecq          {s_gv:>8.0} ms  {:>7.1} MB", mb(std::fs::metadata(&gv_path)?.len()));
    println!("  per-project usearch  {s_pu:>8.0} ms  {:>7.1} MB total", mb(pu_bytes));
    println!("  per-project vecq     {s_pv:>8.0} ms  {:>7.1} MB total\n", mb(pv_bytes));

    // re-index one project of about 700 functions
    let victim = *all.iter().min_by_key(|p| (data.projects[**p].n as i64 - 700).abs()).unwrap();
    let vp = &data.projects[victim];
    let (_, t_global_re) = time(|| {
        for i in 0..vp.n {
            let _ = gu.remove((vp.offset + i) as u64);
        }
        for i in 0..vp.n {
            gu.add((vp.offset + i) as u64, data.vec(vp.offset + i)).unwrap();
        }
        gu.save(gu_path.to_str().unwrap()).unwrap();
    });
    let (_, t_global_re_vecq) = time(|| {
        let mut g = vecq_index();
        // vecq has no cheap in-place update of a slice: cora removes and re-adds keys
        for i in 0..vp.n {
            g.add_keyed((vp.offset + i) as u64, data.vec(vp.offset + i));
        }
        std::fs::write(&gv_path, gv.to_bytes()).unwrap();
    });
    let (_, t_proj_re) = time(|| {
        let idx = build_project_usearch(data, victim);
        idx.save(dir.join("victim.usearch").to_str().unwrap()).unwrap();
    });
    let (_, t_proj_re_vecq) = time(|| {
        let idx = build_project_vecq(data, victim);
        std::fs::write(dir.join("victim.vecq"), idx.to_bytes()).unwrap();
    });
    println!("re-index one project of {} functions (embeddings ready; rewrite its vectors and save):", vp.n);
    println!("  global usearch (remove + add + save whole file)   {t_global_re:>8.0} ms");
    println!("  global vecq    (save whole file; add excluded)    {t_global_re_vecq:>8.0} ms");
    println!("  per-project usearch (build + save own file)       {t_proj_re:>8.0} ms");
    println!("  per-project vecq    (build + save own file)       {t_proj_re_vecq:>8.0} ms\n");

    // load
    let (_, l_gu) = time(|| Index::restore_from_buffer(&std::fs::read(&gu_path).unwrap()).unwrap());
    let (_, l_gv) = time(|| VecqIndex::from_bytes(&std::fs::read(&gv_path).unwrap()).unwrap());
    let (_, l_pu) = time(|| Index::restore_from_buffer(&std::fs::read(dir.join(format!("p{victim}.usearch"))).unwrap()).unwrap());
    let (_, l_pv) = time(|| VecqIndex::from_bytes(&std::fs::read(dir.join(format!("p{victim}.vecq"))).unwrap()).unwrap());
    println!("load (read file and rebuild in memory):");
    println!("  global usearch       {l_gu:>8.1} ms");
    println!("  global vecq          {l_gv:>8.1} ms");
    println!("  one project usearch  {l_pu:>8.1} ms");
    println!("  one project vecq     {l_pv:>8.1} ms\n");

    // query latency
    let tg = targets(data);
    let queries: Vec<(usize, &Vec<f32>)> = tg.iter().flat_map(|p| data.projects[*p].queries.iter().map(move |(q, _)| (*p, q))).collect();
    let gu = Layout::GlobalUsearch(gu);
    let gv = Layout::GlobalVecq(gv);
    let pu = Layout::ProjectUsearch(pu);
    let pv = Layout::ProjectVecq(pv);
    println!("query latency, {} queries, 1 thread (microseconds, median / p95):", queries.len());
    for (name, layout, ask) in [
        ("global usearch, cora over-fetch", &gu, Ask::Cora),
        ("global usearch, filtered search", &gu, Ask::Filtered),
        ("global usearch, adaptive fetch", &gu, Ask::Adaptive),
        ("global vecq, cora over-fetch", &gv, Ask::Cora),
        ("per-project usearch", &pu, Ask::Cora),
        ("per-project vecq", &pv, Ask::Cora),
    ] {
        let mut lat: Vec<f64> = queries
            .iter()
            .map(|(p, q)| {
                let t = Instant::now();
                let r = search(layout, data, *p, q, ask);
                std::hint::black_box(r);
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        println!("  {:<34} {:>8.0} / {:>8.0}", name, pct(&mut lat, 0.5), pct(&mut lat, 0.95));
    }
    Ok(())
}

/// Memory of one layout, measured alone in a fresh process.
fn mem(data: &Data, dir: &Path, which: &str) -> Result<()> {
    let p = targets(data)[3];
    let all: Vec<usize> = (0..data.projects.len()).collect();
    // `data` itself (all vectors) is resident; report the delta of loading the index
    let before = rss_mb();
    let layout = match which {
        "global-usearch" => Layout::GlobalUsearch(Index::restore_from_buffer(&std::fs::read(dir.join("global.usearch"))?)?),
        "global-vecq" => Layout::GlobalVecq(VecqIndex::from_bytes(&std::fs::read(dir.join("global.vecq"))?)?),
        "project-usearch" => Layout::ProjectUsearch(HashMap::from([(p, Index::restore_from_buffer(&std::fs::read(dir.join(format!("p{p}.usearch")))?)?)])),
        "project-vecq" => Layout::ProjectVecq(HashMap::from([(p, VecqIndex::from_bytes(&std::fs::read(dir.join(format!("p{p}.vecq")))?)?)])),
        other => anyhow::bail!("unknown layout {other}"),
    };
    let after = rss_mb();
    let q = &data.projects[p].queries[0].0;
    let _ = search(&layout, data, p, q, Ask::Cora);
    println!("{which:<18} index memory +{:.1} MB (rss {:.0} -> {:.0}, {} projects exist)", after - before, before, after, all.len());
    Ok(())
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let model = StaticModel::from_pretrained(&a[2], None, None, None)?;
    let data = load_data(Path::new(&a[1]), &model)?;
    drop(model);
    let idx_dir = PathBuf::from("/tmp/q_vec/idx");
    match a[3].as_str() {
        "quality" => quality(&data),
        "mono" => mono(&data),
        "perf" => perf(&data, &idx_dir)?,
        "mem" => mem(&data, &idx_dir, &a[4])?,
        other => anyhow::bail!("unknown phase {other}"),
    }
    Ok(())
}
