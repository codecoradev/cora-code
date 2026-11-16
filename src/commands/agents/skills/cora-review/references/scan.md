{{CORA_SKILL_MARKER}}

# Scan the project

Set up ignores first (see "Before the first review: ignore" in SKILL.md): a scan reads every file that is not ignored.

1. Call `cora.scan_prepare` with `{ "batch": 0 }` (optionally `"path"`, inside the project, and `"include"` globs to limit the scan). Note `total_batches`. Cora brings its index up to date first, so each batch carries impact, tests and related code.
2. Judge each batch as in a diff review (answer `user_prompt` following `system_prompt`, return only the JSON array), calling `cora.scan_prepare` with `batch` 1, 2, ... until the last. Deterministic findings come with batch 0.
3. Call `cora.scan_finalize` once with all your findings as `issues` and the batch indexes you actually judged as `batches_judged` (for example `[0, 1, 2]`).
4. Show the user the result.

If you judged fewer batches than `total_batches`, the result says `coverage: "partial"` and `should_block` is null: do not report that as a clean scan. Large projects: scan a subtree with `include`, or only what changed with a `changed_since` revision, instead of skipping batches silently.

Without an agent: `cora scan --no-llm` runs the deterministic pass only.
