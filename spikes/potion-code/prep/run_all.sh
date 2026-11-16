#!/usr/bin/env bash
# Run every backend on every dataset, one process per run (so RSS is per backend).
# usage: run_all.sh <data_dir> <models_dir> <out_dir>
set -euo pipefail
DATA=$1; MODELS=$2; OUT=$3
EVAL=$(dirname "$0")/../target/release/eval
mkdir -p "$OUT"
for ds in ${DATASETS:-cora_hand cora_doc crates_doc py_doc}; do
 for rep in sig body; do
  run() { name=$1; shift; DOCREP=$rep LABEL=$name "$EVAL" "$DATA/$ds" "$@" > "$OUT/$ds.$rep.$name.jsonl"; echo "$ds $rep $name done"; }
  run bm25 bm25
  run hashing hashing
  run nomic nomic
  run potion-code-v2 potion "$MODELS/potion-code-16M-v2"
  run potion-code-v2-int8 potion "$MODELS/potion-code-16M-v2" int8
  run potion-code-v1 potion "$MODELS/potion-code-16M"
  run potion-base-8M potion "$MODELS/potion-base-8M"
  run potion-base-32M potion "$MODELS/potion-base-32M"
 done
done
