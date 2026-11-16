# Spike: potion-code-16M-v2 through model2vec-rs

Throwaway check for issue #746, not part of the cora build (own `[workspace]`).
Question: does the static code embedding `minishlab/potion-code-16M-v2` run
through the `model2vec-rs` crate on a plain CPU, and what does it cost?

## Run

Download `config.json`, `tokenizer.json` and `model.safetensors` of the model
into a directory (33.5 MB in total), then:

```bash
cd spikes/potion-code
cargo build --release
./target/release/potion-code-spike <model_dir> <source_dir> [int8]
```

It cuts the `.rs` files of `<source_dir>` into function-sized chunks, embeds
them, and answers eight sample questions by cosine similarity.

## Result

Machine: 4 vCPU, 16 GB RAM, no GPU. Model: 256 dimensions, 63.5k tokens, stored
as float16 (32.5 MB), loaded as f32 by default.

| Corpus | Threads | Encode time | Throughput | CPU time (user) |
|---|---|---|---|---|
| this repo's `src/`: 2,783 chunks, 1.6 MB | 4 | 158 ms | ~17,600 chunks/s, ~10 MB/s | 0.7 s |
| same | 1 | 575 ms | ~4,800 chunks/s, ~2.8 MB/s | 0.6 s |
| 8,792 crate files: 124,207 chunks, 56.9 MB | 4 | 6.0 s | ~20,700 chunks/s, ~9.5 MB/s | 22.7 s |

| Memory | f32 | int8 (`quantize_int8`) |
|---|---|---|
| after loading the model (RSS) | 89 MB | 42 MB |
| after encoding the 1.6 MB corpus (RSS) | 94 MB | 48 MB |
| vectors for 124k chunks | 127 MB (256 x f32) | same |

- Model load: 150 to 250 ms from a warm file cache.
- A query embeds in about 0.02 ms (8 queries in 0.13 ms).
- Peak RSS values printed by the spike (120 MB on the small corpus, 364 MB on
  the large one) include the spike's own copies of the corpus text and the
  transient f16 to f32 conversion on load; the table above is the steady state.
- Quantizing to int8 halves the resident model for a small speed cost.

## Retrieval sample (not a benchmark)

Eight natural-language questions over this repository's functions, top 3 each:
for six the expected function is in the top 3 (for example "split a diff into
chunks by size budget" finds `split_for_retry` and `retry_budget`, "load the
configuration from a yaml file" finds `parse_profile_yaml` and
`read_yaml_config`). "which files are affected by a change" put `find_affected_tests`
third behind an unrelated helper, and "find dead code" ranked a test fixture
first. The model was trained on Python, Java, JavaScript, Go, PHP and Ruby, not
Rust, and still handles Rust identifiers reasonably here. A proper comparison
against `hashing` and the built-in `pretrained` backend needs the fixed question
set planned in #746.
