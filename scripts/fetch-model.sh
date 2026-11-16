#!/usr/bin/env bash
# Fetch the code embedding model that release binaries bundle (feature
# `bundled-model`) into a directory, verifying every file against its pinned
# SHA-256. Usage: scripts/fetch-model.sh <dir>
#
# The pins below must equal src/embed/model_store.rs; a unit test checks that.
set -euo pipefail

REVISION="e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b"
BASE="https://huggingface.co/minishlab/potion-code-16M-v2/resolve/${REVISION}"

dir="${1:?usage: fetch-model.sh <dir>}"
mkdir -p "$dir"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() { # name size sha256
  local name="$1" size="$2" want="$3" path="$dir/$1"
  if [ -f "$path" ] && [ "$(wc -c < "$path" | tr -d ' ')" = "$size" ] && [ "$(sha256 "$path")" = "$want" ]; then
    echo "ok (cached) $name"
    return
  fi
  curl -sSfL --retry 3 --retry-delay 2 -o "$path" "$BASE/$name"
  [ "$(wc -c < "$path" | tr -d ' ')" = "$size" ] || { echo "$name: wrong size" >&2; exit 1; }
  [ "$(sha256 "$path")" = "$want" ] || { echo "$name: checksum mismatch" >&2; exit 1; }
  echo "ok $name"
}

fetch config.json 59 148e5691a6fcc553437156859701fba017a1ba5d340b170f17e0f3668fb861a7
fetch tokenizer.json 1024340 107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45
fetch model.safetensors 32490072 75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c
