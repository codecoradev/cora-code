---
name: cora-code
description: Searches this project's code by meaning, finds who calls a function, what a change affects and which tests to run, and reviews a branch with cora before a pull request, using cora's local code index. Use before reading or changing code you do not know, when asked where something is implemented or what a change would break, and before opening a pull request.
---

{{CORA_SKILL_MARKER}}

# Code search with cora

cora keeps a local index of the project (symbols, call graph, a code embedding model). It answers in milliseconds and costs no model tokens, so use it before guessing file names or grepping for words. The index is built from the project files; cora brings it up to date itself when it reviews or scans.

## Find code

- A question in words ("where do we retry a failed request"): `cora_brain_search` (CLI: `cora brain "<question>"`). It fuses keyword, meaning and call-graph signals and returns symbols with file and line.
- A name you already know: `cora_search_symbols` (CLI: `cora explore <name>`; a multi-word query also lists matches by meaning).

Read the files a result points to before you rely on it: the index finds candidates, it does not replace reading.

## Before you change something

1. `cora_find_callers` (`cora callers <name>`) lists who calls it.
2. `cora_find_impact` (`cora impact <name>`) lists what breaks if it changes, level by level.
3. `cora_find_affected_tests` (`cora affected`) lists the tests to run for changed files.

More commands (`trace`, `dead-code`, `query`, `routes`, `arch`) and their flags: `references/tools.md`.

## First time in a project

0. Make sure cora is installed (`cora --version`). Install it from the GitHub release binary, never by building it: the release binary already contains the model and works offline, a source build does not. How, and what to run afterwards: `references/install.md`.
1. Decide what to ignore before anything is indexed or reviewed: dependencies, build output and generated code add noise and untrusted text. cora ignores the usual ones by default; read `references/ignore.md` for what to add.
2. Run `cora index` in the project root (seconds; the first run may download a 33 MB model once, release binaries already contain it). If a tool says there is no index, run it again.

## Before you open a pull request

You must review your own branch with cora as the reviewer before the pull request is opened: use the `cora-review` skill on `git diff <base>...HEAD`. Your own model judges; cora adds its deterministic checks and the code intelligence above. If the project has no `cora review` in its CI, suggest adding it (the `cora-review` skill explains how).

## References

- `references/tools.md`: every index command and MCP tool, with flags.
- `references/ignore.md`: what to ignore before indexing, reviewing and scanning, and how.
- `references/install.md`: install cora from the release binary, not a build, and what to run after.
