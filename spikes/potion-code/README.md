# Spike: local code embeddings for `cora brain` (#746)

Throwaway crate, not part of the cora build (own `[workspace]`). It answers one
question: **would a Model2Vec static code embedding (`potion-code-16M-v2`,
through the `model2vec-rs` crate) retrieve code better than what cora ships
today, and what does it cost on a plain CPU?**

Everything here runs offline after the model files are downloaded; no API key,
no GPU. Full generated tables are in [`RESULTS.md`](RESULTS.md).

## What was compared

| Backend | What it is |
|---|---|
| `bm25` | keyword baseline over identifier-split tokens (cora's own FTS5 differs slightly, see limits) |
| `hashing` | cora's default 256-d hashing-trick embedding (cora's own code, included from `src/embed/`) |
| `nomic` | cora's `pretrained` backend, 768-d static vocabulary distilled from nomic-embed-code (cora's own code and vendored vectors) |
| `potion-code-v2` | `minishlab/potion-code-16M-v2`, 16.2M parameters, 256-d, MIT |
| `potion-code-v1`, `potion-base-8M`, `potion-base-32M` | older code model and general-purpose static models, for contrast |
| `*-int8` | the same model quantized with `quantize_int8` |
| `bm25+X@w` | cora-style reciprocal rank fusion (k = 60) of `bm25` (weight 1) and `X` (weight `w`) |

Two document forms: **name + signature** (exactly what cora embeds and indexes
today, see `embed_project`) and the **whole function body**.

## Data (all built offline by `prep/extract.py`)

| Set | Searched | Queries |
|---|---|---|
| `cora_hand` | 2,791 functions of this repository | 39 questions written by hand, with the acceptable answers |
| `cora_doc` | same | 400 doc comments (first paragraph) of functions, doc removed from the document |
| `crates_doc` | 5,028 functions of 8 Rust crates | 600 doc comments |
| `py_doc` | 2,374 Python stdlib functions | 600 docstrings |
| `*_kw` | as above | the same queries cut to their first 3 content words |

Each doc-comment query has one relevant function; every other function is a
distractor (test functions included, as in cora's index). A second query form
masks the function's own name to rule out matching on the name. Metrics:
MRR@10, recall@1/5/10, NDCG@10, with bootstrap 95% intervals and paired
bootstrap differences against `bm25` alone.

## Results (documents = name + signature, query as written, MRR@10)

| Method | cora_hand | cora_doc | crates_doc | py_doc |
|---|---|---|---|---|
| bm25 | 0.231 | 0.415 | 0.234 | 0.306 |
| hashing alone | 0.224 | 0.221 | 0.084 | 0.063 |
| bm25 + hashing @0.5 (cora today, half weight) | 0.285 | 0.445 | 0.206 | 0.213 |
| nomic alone | 0.190 | 0.286 | 0.090 | 0.161 |
| bm25 + nomic @0.5 | 0.222 | 0.375 | 0.180 | 0.227 |
| **potion-code-v2 alone** | **0.346** | 0.490 | 0.303 | **0.398** |
| bm25 + potion-code-v2 @1 | 0.284 | **0.503** | 0.302 | 0.349 |
| bm25 + potion-code-v2 @2 | 0.294 | **0.507** | **0.303** | 0.364 |
| potion-code-v1 alone | 0.266 | 0.440 | 0.288 | 0.368 |
| potion-base-32M alone (general, 2x the size) | 0.249 | 0.414 | 0.226 | 0.289 |

Difference in MRR@10 against `bm25` alone, with the paired 95% interval:

| Method | cora_hand | cora_doc | crates_doc | py_doc |
|---|---|---|---|---|
| potion-code-v2 alone | +0.115 (+0.044 to +0.203) | +0.075 (+0.040 to +0.113) | +0.068 (+0.039 to +0.096) | +0.092 (+0.062 to +0.124) |
| bm25 + potion-code-v2 @1 | +0.053 (+0.015 to +0.104) | +0.089 (+0.065 to +0.112) | +0.068 (+0.048 to +0.087) | +0.043 (+0.024 to +0.061) |
| bm25 + hashing @0.5 | +0.054 (+0.005 to +0.115) | +0.030 (-0.003 to +0.059) | -0.028 (-0.047 to -0.008) | -0.093 (-0.115 to -0.071) |
| bm25 + nomic @0.5 | -0.009 (-0.045 to +0.028) | -0.040 (-0.073 to -0.008) | -0.054 (-0.076 to -0.033) | -0.079 (-0.101 to -0.056) |

What this shows:

1. **potion-code-v2 is a net gain on all four sets**, alone and fused, and the
   intervals exclude zero. It also wins with keyword-style queries
   (`*_kw`: 0.398 / 0.195 / 0.299 alone against 0.341 / 0.147 / 0.271 for
   `bm25`) and with the function name masked.
2. **cora's current vectors are not**: `hashing` and `nomic` at the current half
   weight are neutral on this repository and harmful on the other Rust crates
   and on Python (down to -0.093 MRR@10). Halving the static embedding's weight,
   the stopgap in `brain_search_report`, is not enough; lowering it to 0.25
   brings the loss to about zero on the crates and still costs -0.04 on Python.
3. **Fusion weight**: with `potion-code-v2`, a dense weight of 1 to 2 beats 0.5
   (cora_doc 0.503-0.507 against 0.481). Alone, it is as good as the fusion on
   crates and better on `cora_hand` and `py_doc`.
4. **Name + signature is the right thing to embed**: for `potion-code-v2` it
   beats whole bodies (alone: 0.346 against 0.180 on `cora_hand`, 0.490 against
   0.396 on `cora_doc`, 0.303 against 0.280, 0.398 against 0.381) and is about
   2.4x faster to embed. No change to what cora indexes is needed.
5. **Code-specific matters, size does not**: `potion-code-v2` beats
   `potion-code-v1` and the general `potion-base-32M`, which has twice the
   parameters.
6. **int8 loses nothing that matters**: MRR@10 differs by at most 0.003 from
   float32 over every run and method, and the resident model halves.
7. It was **not trained on Rust**, and still beats the keyword baseline by
   +0.068 on 8 Rust crates.

## Cost (4 vCPU, 16 GB RAM, no GPU; 5,028 functions of the 8 crates)

| Backend (name + signature docs) | Load | Embed all | RSS added by the backend | Index vectors |
|---|---|---|---|---|
| hashing, 256-d | 0 ms | 88,600 docs/s | baseline | 5.1 MB |
| nomic, 768-d | 5 ms | 118,000 docs/s | +3 MB at load, about +45 MB once its vectors are touched | 15.4 MB |
| potion-code-v2, f32 | 130 ms | 63,000 docs/s (~80 ms for all) | about +80 MB | 5.1 MB |
| potion-code-v2, int8 | 210 ms | 72,000 docs/s | about +35 MB | 5.1 MB |

A query embeds in about 0.01 ms for every backend. Whole-body documents embed
2.4x slower (26,500 docs/s for `potion-code-v2`). On the earlier smoke test
(`src/bin/smoke.rs`), 124,207 function chunks (56.9 MB of text) encoded in 6.0 s
on 4 threads (22.7 s of CPU), 2.8 MB/s on one thread. The model is 32.5 MB on
disk (float16) plus a 1 MB tokenizer; the build links `model2vec-rs` (MIT).
"RSS added" is the process total minus the hashing run on the same data. The
peak is higher than the resident size (about +25 MB for float32, about +70 MB
for int8) because the model is converted to float32 at load before it is
quantized.

## Does this harness reflect production?

`prep/prod_brain.py` runs the real `cora brain` (FTS5 + hashing vectors + graph,
RRF, natural-language mode) on the same queries: MRR@10 is 0.217 on `cora_hand`
and 0.388 on `cora_doc`, against 0.231 and 0.415 for the `bm25` baseline here.
The harness is therefore close to, and slightly generous towards, the keyword
part of production.

## Limits, stated plainly

- Doc-comment and hand-written questions are a proxy for real agent questions.
  The 39 hand questions were written by the author of the code, so they are
  easier than a stranger's; their intervals are wide, but the direction matches
  the 1,600 automatic queries.
- The `bm25` baseline tokenizes camelCase and snake_case on both sides; cora's
  FTS5 only splits on the query side. Production keyword search is slightly
  weaker, so real gains from a better vector signal would be a little larger.
- The graph signal of `brain` is not part of the harness.
- The model card reports the same direction on a standard benchmark (CoIR,
  NDCG@10: potion-code-v2 39.1, BM25 42.3, hybrid 43.4, CodeRankEmbed
  transformer 59.1): a static model is far below a transformer. A transformer
  (`jina-embeddings-v2-base-code`, 161M parameters) **could not be tested here**:
  its ONNX files are served from a host the sandbox does not allow. `probe/`
  builds and loads it through `fastembed` on a machine with open access; the
  ONNX Runtime shared library can be taken from the `onnxruntime` Python wheel
  and passed with `ORT_DYLIB_PATH`.
- Training data licenses of the model were not reviewed; the model card says MIT.

## Run it

```bash
# model files (config.json, tokenizer.json, model.safetensors) for each model
#   huggingface.co/minishlab/<name>/resolve/main/<file>
cd spikes/potion-code && cargo build --release
python3 prep/extract.py rust  /tmp/data/cora_doc 400 ../../src
python3 prep/extract.py hand  /tmp/data/cora_hand prep/cora_questions.json ../../src
python3 prep/extract.py rust  /tmp/data/crates_doc 600 <crate>/src ...
python3 prep/extract.py python /tmp/data/py_doc 600 /usr/lib/python3.11/json ...
python3 prep/make_kw.py /tmp/data/cora_doc /tmp/data/cora_doc_kw   # optional
prep/run_all.sh /tmp/data /tmp/models /tmp/results
python3 prep/report.py /tmp/results > RESULTS.md
python3 prep/prod_brain.py ../../target/debug/cora /tmp/data/cora_doc ../..   # needs `cora index`
./target/release/smoke <model_dir> <src_dir> [int8]
```
