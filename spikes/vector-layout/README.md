# Spike: one vector file for all projects, or one per project? (#746 follow-up)

Throwaway crate, not part of the cora build (own `[workspace]`). It answers:

1. Does cora's single global vector file (every project's vectors in one
   usearch index, results filtered to the current project afterwards) hurt the
   other projects, or itself?
2. Would one file per project, with the quantized `vecq` store, be better, and
   by how much in quality, speed, disk and memory?

It uses real data and cora's real vector configuration: **176 projects** (every
Rust crate of the local cargo registry with at least 150 functions), **127,400
functions** embedded as `name + signature` with the real `potion-code-16M-v2`
model, and doc-comment queries whose answer is one function of the same
project (12 target projects, 696 queries). usearch is configured as cora does
(256 dimensions, cosine, f32, default HNSW parameters), vecq as cora does
(residual, seed 42). Raw outputs are in `results/`.

## What was compared

| Layout | How |
|---|---|
| global usearch, cora over-fetch | cora today: ask the whole index for `max(5 x 2 x limit, 50)` neighbours, keep this project's ids, take `2 x limit` |
| global usearch, fetch x10 / adaptive | the same index, asked for more (fixed 1,000, or doubled until enough hits) |
| global usearch, filtered search | usearch's own `filtered_search` on the project |
| global vecq | the same global layout on the quantized scan store |
| per-project usearch / per-project vecq | one index file per project, no filtering |
| exact | brute-force cosine inside the project: the ceiling any index can reach |

## 1. Is the shared file "poisoning" other projects?

The vectors themselves do not affect each other: inserting a vector never
changes another one, and a result is filtered to its project. What goes wrong
is the **retrieval**: the nearest neighbours of a question come from all
projects, and cora asks for a fixed number of them before it filters. The more
projects share the file, the fewer of the candidates belong to the one being
searched.

MRR@10 and recall@10 of the vector signal alone, against the exact ceiling
(0.366 / 0.667 with `--limit 10` and 20 candidates; the sets differ per row):

| Projects in the file | Vectors | cora over-fetch, limit 10 | limit 20 (CLI default) | limit 3 (what `cora review` asks) |
|---|---|---|---|---|
| 12 | 8,990 | 0.354 / 0.649, 0.4% queries empty | 0.360 / 0.665 | n/a |
| 40 | 20,347 | 0.343 / 0.611, 1.3% empty | 0.360 / 0.659 | 0.319 / 0.486, 6.2% empty (ceiling 0.352 / 0.550) |
| 100 | 59,878 | 0.316 / 0.526, 6.8% empty | n/a | n/a |
| 176 | 127,400 | **0.293 / 0.473, 14.2% empty, 8 of 20 candidates** | **0.326 / 0.559, 6.9% empty** | **0.250 / 0.361, 26.4% empty** (ceiling 0.352 / 0.550) |

So at 176 projects a search loses 11 to 29% of its MRR and 16 to 34% of its
recall depending on the limit, and up to a quarter of the questions find no
vector candidate at all. `cora review` asks for 3 to 5 results, the worst case.
cora's own database of one user had 339 project rows for about 20 live
repositories (#725), so this is not an exotic size.

Fixing the query restores the ceiling on the global file (adaptive fetch 0.358,
filtered search 0.360 at 176 projects), **but at a cost** (section 2).
A per-project file has no dilution by construction and matches the ceiling
(0.356 usearch, 0.360 vecq at 12 projects; the same at any count).

A second, separate cause of waste, seen in this session's real data directory:
cora never removes a vector (there is no `remove` call outside tests), and
re-indexing a file gives its symbols new ids. **30% of the vectors in the global
hashing index (2,241 of 7,594) and 8% in the code-model index belonged to
symbols that no longer exist**, after a few hours of use. They fill the
over-fetch and the file, and nothing ever frees them.

## 2. Cost of each layout (all 176 projects, 127,400 vectors, 4 vCPU, one thread)

| | global usearch | global vecq | per-project usearch | per-project vecq |
|---|---|---|---|---|
| build everything | 33.9 s | 1.8 s | 9.5 s | **1.5 s** |
| disk | 149.4 MB | 34.7 MB | 149.3 MB | **34.7 MB** |
| re-index one project (689 functions) | 306 ms (rewrites 149 MB) | 45 ms (rewrites 35 MB) | 35 ms | **8 ms** |
| load a file in a fresh process | 195 ms | 42 ms | 0.6 ms | **0.1 ms** |
| memory added by loading | **+148.5 MB** | +24.1 MB | +2.5 MB | **~0 MB** |
| query, cora over-fetch (median / p95) | 0.21 / 0.38 ms | 13.8 / 15.9 ms | 0.03 / 0.07 ms | 0.06 / 0.16 ms |
| query, filtered or adaptive | 5.5 / 33.5 ms, 2.9 / 24.4 ms | | | |

The global file is the expensive one: the 149 MB index is read and resident in
every `cora brain` process whichever project it searches, and every project
that is re-indexed rewrites everybody's vectors. The only way to get the right
results from it (filtered or adaptive search) makes a query roughly 50 to 170 times
slower than a per-project file.

## 3. vecq or usearch inside one project (a monorepo)

Per-project files do not shrink a project that is itself huge. Treating the
first N crates as one project (no filtering), 20 results, vector signal alone:

| Vectors in the project | exact | usearch | vecq residual | usearch latency | vecq latency |
|---|---|---|---|---|---|
| 8,990 | 0.285 / 0.514 | 0.279 / 0.504 | 0.283 / 0.517 | 0.20 ms | 0.85 ms |
| 20,347 | 0.256 / 0.440 | 0.243 / 0.414 | 0.254 / 0.443 | 0.25 ms | 2.1 ms |
| 59,878 | 0.191 / 0.356 | 0.170 / 0.316 | 0.202 / 0.358 | 0.31 ms | 6.3 ms |
| 127,400 | 0.161 / 0.289 | 0.137 / 0.247 | 0.169 / 0.295 | 0.32 ms | 12.8 ms |

(MRR@10 / recall@10.) usearch's approximate search loses 2 to 15% of the
recall at these sizes; vecq's quantized scan keeps up with the exact ceiling at
every size, at 4.3x smaller files, 15 to 18x faster builds and a latency that
grows linearly: about 2 ms at 20,000 symbols (a large repository) and 13 ms at
127,000.

## Conclusion

- **The single global file is not valid at cora's scale.** Other projects crowd
  the current one out of the candidates (up to 29% of MRR and a quarter of
  queries empty), vectors of deleted symbols are never freed, one index is read
  into memory by every process, and every re-index rewrites all projects.
- **One file per project, with vecq, wins on every axis measured**: quality at
  the exact ceiling, 4.3x smaller, 300 to 400x faster to load, 6 to 9x faster to
  re-index, no resident memory, no cross-project locking, and a project's file can be
  rebuilt from scratch (8 ms for 689 symbols), which removes orphans and the
  need for per-symbol fingerprints.
- usearch stays useful only for single projects beyond about 100,000 symbols,
  where its 0.3 ms query beats vecq's 13 ms; its recall is lower there.

## Limits

- The projects are Rust crates, similar in domain and style. Repositories of
  different kinds are easier to tell apart, so dilution would be somewhat
  smaller; the orphan effect and the cost numbers do not depend on that.
- Only the vector signal was measured, not the whole `cora brain` fusion, where
  keyword and graph signals soften a weak vector list.
- Timings are single-threaded on one machine; vecq latency is linear in the
  project size. The re-index row measures writing vectors that are already
  embedded (embedding itself is about 60,000 symbols per second).
- Orphan counts come from one development data directory, not a survey.

## Run it

```bash
python3 prep/make_projects.py /tmp/projects 150 ~/.cargo/registry/src/*/*/src
cargo build --release
./target/release/vector-layout-spike /tmp/projects <potion_model_dir> quality   # LIMIT=10|20|3
./target/release/vector-layout-spike /tmp/projects <potion_model_dir> perf
./target/release/vector-layout-spike /tmp/projects <potion_model_dir> mem global-usearch  # also global-vecq, project-usearch, project-vecq
./target/release/vector-layout-spike /tmp/projects <potion_model_dir> mono
```
