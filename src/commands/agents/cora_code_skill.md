---
name: cora-code
description: Search this project's code by meaning, find who calls a function and what a change affects, and pick the tests to run, using cora's local code index. Use before reading, changing or explaining code you do not know, when asked where something is implemented, how a feature works, or what a change would break.
---

# Code search with cora

cora keeps a local index of the project (symbols, call graph, a code embedding model). Use it instead of guessing file names or grepping for words. It answers in milliseconds and costs no model tokens.

## Find code

- **A question in words** ("where do we retry a failed request", "how are findings stored"): `cora_brain_search` (CLI: `cora brain "<question>"`). It fuses keyword, meaning and call-graph signals and returns symbols with file and line.
- **A name you already know**: `cora_search_symbols` (CLI: `cora explore <name>`; a multi-word query also lists matches by meaning).

Read the files it points to before you rely on a result: the index finds candidates, it does not replace reading.

## Before you change something

1. `cora_find_callers` (`cora callers <name>`, or `Type::name` for one type) lists who calls it.
2. `cora_find_impact` (`cora impact <name>`) lists what breaks if it changes, level by level.
3. `cora_find_affected_tests` (`cora affected <file>`) lists the tests to run for a changed file.
4. `cora trace <name> --direction incoming|outgoing` follows the call chain.
5. `cora dead-code` lists unused functions.

## If it says there is no index

Run `cora index` in the project root (seconds; the first run downloads a 33 MB model once). Re-run it after large changes; a project's index is rebuilt from its files.

## Reviews

To review a diff, use the `cora-review` skill.
