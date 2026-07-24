# Changelog

All notable changes to cora-code are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Dart symbol indexing.** `cora index` now extracts classes, mixins, enums, extensions, functions, getters, and typedefs from `.dart` files. Closes #373.

## [0.8.1] - 2026-07-24

### Fixed

- crates.io publish (503 transient on v0.8.0)


## [0.8.0] - 2026-07-24

### Highlights

- **Brain Mode — hybrid code search.** `cora brain <query>` combines FTS5 keyword search, usearch vector similarity (HNSW), and graph BFS proximity into a single ranked result set via RRF fusion (k=60). Index-time embeddings use a zero-dependency static token method (256d) — no model download, no GPU.
- **tree-sitter AST extraction + call edges.** Schema v3 adds an `edges` table storing caller→callee relationships. When `cora index` runs with `--features tree-sitter`, it extracts function calls from AST nodes, enabling `cora trace` and `cora arch`.
- **`cora trace` and `cora arch` commands.** Trace symbol call chains (depth-limited) and display architecture overview (module breakdown, edge types, top connectors) from the indexed call graph.
- **Static token embedding engine.** Zero-dependency bag-of-tokens hashing (256d) for code symbol embeddings — suitable for near-duplicate detection and semantic search without external models.
- **Global index directory.** The symbol database migrated from `.cora/graph.db` (per-project) to `~/.codecora/cora-code/graph.db` (per-user), shared across all projects.
- **Renamed `cora-cli` → `cora-code`.** Binary is now `cora`, crate is `cora-code`.

### Added

- **Phase 3 — Brain Mode** (#362)
  - `CodeVectorIndex` — persistent usearch HNSW vector index with fs2 file locking, key↔symbol mapping, and disk serialization
  - `brain_search()` — hybrid search: FTS5 + usearch KNN (cosine, top-50) + graph BFS (depth-2 from FTS hits) → RRF k=60 fusion
  - Schema v4: `embedding_tier`, `embedding_dims`, `embedding_model`, `last_embedded_at` columns on `projects` table
  - Index-time embedding: all symbols embedded via static tokens during `cora index`
  - CLI: `cora brain <query> [--json] [--limit N]`
  - MCP tool: `cora.brain_search` — semantic code search for AI coding agents
- **Phase 2C — `cora trace` and `cora arch`** (#358)
  - `cora trace <symbol>` — trace call chains from a symbol (depth-limited BFS on call edges)
  - `cora arch` — architecture overview: module breakdown, edge types, top connectors
- **Phase 2 — tree-sitter AST extraction + Schema v3** (#356)
  - tree-sitter AST node extraction for Rust, Python, JavaScript, TypeScript, Go, Java
  - Schema v3: `edges` table (caller_id, callee_id, edge_type) storing call relationships
  - Gated behind `--features tree-sitter` (default build does not include tree-sitter)
- **Phase 1 — Static token embedding engine** (#354)
  - Bag-of-tokens hashing: 256d vectors from code text, zero external dependencies
  - Pre-trained nomic-embed-code vocabulary included (768d, reserved for Phase 5)
  - `tokenize_code()`, `embed_code()`, `cosine_similarity()` public API
- **Global index migration** (#355)
  - Symbol database moved from `.cora/graph.db` to `~/.codecora/cora-code/graph.db`
  - `CODECORA_HOME` env var override for custom data directory
- **Binary rename** (#338)
  - Crate renamed `cora-cli` → `cora-code`
  - Binary name: `cora`

### Changed

- **Docs website** — adopted `@codecora/theme` + VitePress base `/cora/docs/`, retired standalone LandingPage (#348)
- **Uteke memory integration** — removed from user-facing docs (implementation exists but undocumented until API stabilizes) (#367)

### Fixed

- **False positives suppressed in `sec-hardcoded-url` and `crypto/hardcoded-secret` rules** (#369, closes #357, #364)
  - `post_match_filter()` added to `builtin.rs` — filters matches in XML/SVG `xmlns` attributes, Rust docstrings, config files, and bare identifiers
  - Integrated into `security_scanner.rs` scan loop — all security scanner matches now pass through `post_match_filter`
  - Docker hostname regex (`DOCKER_HOST_RE`) fixed — `\d+` now correctly matches port digits
  - 31 targeted unit tests added for false positive suppression
- **CI clippy lints** — `map_or(false, ...) → idiomatic `is_some_and(...)` (#369)

### Stats

- 56 files changed, +46,948 / -2,050 lines since v0.7.0
- 11 PRs merged

## [0.7.0] - 2026-07-16

### Highlights

- **Deeper, token-economical cross-file review.** Reviews now resolve **who calls the changed code** (inbound / blast-radius), not just what the changed code calls — so breaking signature/type changes can be flagged. Bounded scanning + thin slices + a signature-only budget fallback keep token cost low.
- **Config is now validated at load time.** Out-of-range values (e.g. `temperature: 5`) and misspelled keys (`quailty_gate`) fail loudly instead of being silently ignored.
- **Markdown false positives suppressed.** Findings inside fenced code blocks (a `git push` in a fenced `bash` block flagged as SQL injection) are now dropped across all finding sources.
- **Performance, security, and correctness fixes** across the scan/review pipeline (10 perf bottlenecks, 2 CVE bumps, 8+ silent-corruption and best-practice bugs).

### Added

- **Inbound caller (blast-radius) resolution.** A new context-chain phase resolves call-sites of functions/types defined or modified in the diff, so breaking changes to their signatures surface their consumers. Gated by new `review.context_chain.include_callers` (default `true`); uses gitignore-aware walking and is bounded (≤400 files, ≤3 call-sites/symbol), injecting only the call line + 1 line of context. New `ContextPriority::CallerSite`.
- **Definition extraction** (`extract_definitions_from_diff`) for Rust/Python/JS-TS/Go/Java-Kotlin — detects functions/types *declared* in the diff, feeding caller resolution. Rust `mod foo;` and Java `import com.example.*` wildcards are now extracted correctly (#73, #72).
- **Signature-only budget fallback.** When the token budget can't fit a full function/type body, a thin signature slice (up to `{`) is injected instead of skipping the entry entirely (~3–5× more symbols under the same budget).
- **`Config::validate()`** (#94) — rejects out-of-range/unsupported values at load: `temperature` (0.0–2.0), `max_tokens`/`timeout` (≥1), `max_tokens_param`, `response_format`, `output.format`, `hook.mode`/`on_violation`/`min_severity`, and `provider.base_url` scheme. Multiple errors are aggregated into one message.
- **`Profile::validate()`** (#81) — focus `weight` must be 1–10, and `action`/`tone`/`detail_level` must be recognized values.
- **`deny_unknown_fields`** on all config sections (#80) — misspelled YAML keys are rejected at parse time.

### Changed

- **`CategoryAction` enum** (#57) — `quality_gate.categories.*.action` is now a case-insensitive enum (`block`/`warn`/`ignore`); a typo like `blok` fails loudly at config load instead of silently becoming blocking.
- **Disabled quality gate never fails** (#58) — `evaluate()` forces `Pass` when `enabled: false`.
- **`context_chain.max_context_tokens` default** raised 3000 → **5000**.
- **`issue_type`** serializes consistently as `issue_type` (#48); `type` retained as a deserialize alias.
- **`Severity::from_str_lossy`** uses `eq_ignore_ascii_case` (no allocation) (#10).

### Fixed

- **Markdown fenced-code-block false positives** (#329) — findings inside fenced code blocks (triple-backtick / triple-tilde) in `.md`/`.mdx`/`.markdown` files are dropped across all finding sources (security/secrets/rules scanners + LLM). Fence state is tracked across full hunk context, so it works even when only the block body was edited.
- **Cross-file resolver used the wrong ignore list** — `review.rs` passed `ignore.rules` (finding-type strings) instead of `ignore.files` (`target/**`, `node_modules/**`); the resolver could inject build-artifact code. Now uses `ignore.files`.
- **Test-file detection over-match** (#87) — `is_test_file` is path-segment aware; `latest`, `aspect`, `attestation` are no longer mistaken for test files.
- **Directory glob excludes over-permissive** (#66) — `src/` matches only at segment boundaries (`mysrc/` no longer caught).
- **Token estimation** (#68) — non-empty content returns ≥1 token (was 0 under integer division).
- **DB size** (#23) — `index_stats` queries `PRAGMA page_size` instead of assuming 4096 bytes.
- **Project-sync workflow** — a merged PR referencing issues via `Refs #N` (not `Closes #N`) no longer fails the `sync` check.
- **10 scan/review performance bottlenecks** (#335) — precompiled regex, batched DB queries, early cutoffs, file-content cache, single reused Tokio runtime, single-transaction prune, etc.
- **Security:** bumped `anyhow` 1.0.102 → 1.0.103 (RUSTSEC-2026-0190) and `crossbeam-epoch` 0.9.18 → 0.9.20 (RUSTSEC-2026-0204).
- Various silent-data-corruption bugs resolved (#333): severity sort, security-findings fallback, deterministic debt-snapshot hashing, debt-trend math, config precedence, `context_chain` merge, and hook-install composition.

## [0.6.2] - 2026-06-21

### Fixed — Token Usage Tracking

- **`tokens_used` is no longer always `None` in review and scan responses.**
  - `parse_review_response` and `parse_scan_response` previously discarded the `usage` object returned by the LLM API, hardcoding `Ok((..., None))`. Token counts and cost estimates were silently dropped.
  - `chat_completion` now returns `(content, Option<Usage>)` and the parse functions thread `usage` through as `TokenUsage`. All call sites updated.
  - `ReviewResponse.tokens_used` and `ScanResponse.tokens_used` now report real values when the provider supplies them.

- **`cora review --stream` now collects token usage.**
  - The streaming path (`chat_completion_stream`) previously only accumulated `delta.content` and ignored the `usage` field. It now sends `stream_options: { include_usage: true }` and parses `usage` from the final SSE chunk (top-level or nested in `choices[0].delta.usage`).
  - Token counts are now reported correctly for both streaming and non-streaming review.

- **`cora scan` multi-batch token accumulation.**
  - When scanning multiple batches, `total_tokens` was overwritten by each batch instead of accumulated. Only the last successful batch's tokens were reported.
  - Token usage now accumulates across all batches (`input_tokens`, `output_tokens`, and `estimated_cost_usd` are summed).

### Changed — Code Quality

- **Extracted magic numbers into named constants.**
  - `scan.rs`: the hardcoded batch size fallback `20` and token budget `60_000` are now `DEFAULT_MAX_FILES_PER_BATCH` and `DEFAULT_BATCH_TOKEN_BUDGET`.
- **`Usage` struct now accepts camelCase aliases.**
  - Some providers (e.g. Azure OpenAI, certain third-party gateways) return `promptTokens` / `completionTokens` / `totalTokens` instead of snake_case. Both forms are now accepted via `#[serde(alias = ...)]`.

### Tests

- Added 4 regression tests for token usage threading: `parse_review_preserves_usage_when_provided`, `parse_review_returns_none_usage_when_not_provided`, `parse_scan_preserves_usage_when_provided`, `usage_to_token_usage_maps_fields_correctly`.

## [0.6.1] - 2026-06-17

### Fixed — Scan

- **`cora scan` no longer aborts on non-JSON LLM responses (#316)**
  - Detect non-JSON responses early (provider error pages, rate-limit bodies, empty responses, prose wrappers) and surface the raw response prefix (first 512 bytes) in the error message so users can diagnose the cause.
  - Per-batch parse failures are now **non-fatal by default**: the failing batch is skipped with a `warn`-level log and a stderr warning listing the affected files, and the scan continues with the remaining batches. Set `--no-continue-on-batch-error` to restore the old abort behavior.
  - Added `--batch-files <N>` flag (default: 20) to control the maximum number of files per LLM batch — lower it to work around provider token limits or rate-limit errors on large scans.
  - Truncated-JSON and general parse errors now include the raw response prefix for easier debugging without `--verbose`.

### Fixed — Review

- **`cora review` no longer exits 2 when severity filtering removes all blocking findings (#312)**
  - Recompute `should_block` against the **filtered** issue list (after `--severity` filtering) so the exit code matches the SARIF/pretty output the user sees.
  - Extracted exit-code logic into `compute_exit_code()` helper (pure function) with 8 unit tests covering gate pass/fail, CI mode, and hook `block` vs non-`block` modes.
  - Applies to both the single-chunk and auto-chunked (`--auto-chunk`) review paths.

### Fixed — Install (macOS)

- **macOS installer now strips Gatekeeper quarantine attributes (#313)**
  - Prebuilt macOS binaries (`aarch64-apple-darwin`) are not Apple-notarized. When downloaded directly, macOS attaches `com.apple.quarantine` / `com.apple.provenance` xattrs and kills the binary with `Killed: 9` on first launch.
  - `install.sh` now runs `xattr -dr` for both attributes on the installed binary on macOS (best-effort, non-fatal).
  - Added a prominent `<details>` block in the README install section explaining the symptom, the manual `xattr` workaround for users who download the binary directly, and the `cargo` / Homebrew alternatives.

### Changed — Docs

- **Install section now warns about multiple distribution channels (#314)**
  - Recommends a single install method per platform and lists the supported channels (installer script, `cargo`, pre-built binaries).
  - Adds a `which -a cora && cora --version` check snippet and guidance for removing stale copies when more than one `cora` is on `PATH` (e.g. `~/.local/bin` vs `~/.cargo/bin` vs npm global).
  - Cross-links the original issue for background.

## [0.6.0] - 2026-06-14

### Added — Code Intelligence

- **`cora index`** — persistent SQLite symbol index with FTS5 (#264)
  - Regex-based definition extraction for 13 languages
  - Incremental reindex via SHA-256 file fingerprints
  - `--stats`, `--prune`, `--rebuild`, `--watch` flags
  - Database: `.cora/index.db`

- **`cora explore`** — search the symbol index (#265)
  - FTS5 full-text search with bm25 ranking
  - Filter by `--kind`, `--file`, `--language`
  - JSON output mode

- **`cora callers` / `cora impact`** — call graph analysis (#266)
  - Reverse call graph traversal (who calls this?)
  - Forward impact analysis (what breaks if changed?)
  - Depth-limited traversal

- **`cora affected`** — test file selection (#267)
  - Find tests affected by source changes
  - Call graph + naming convention strategies
  - stdin support for `git diff --name-only | cora affected --stdin`

- **Language expansion** — 6 → 13 languages (#268)
  - Ruby, PHP, Swift, Scala, Lua, Zig

- **`cora index --watch`** — auto-sync file watcher (#269)
  - Poll-based incremental reindex (2s interval)
  - No extra dependencies

### Added — MCP Server (14 tools)

- **Phase 1: Code Intelligence** (#284) — 5 new MCP tools:
  `cora.search_symbols`, `cora.find_callers`, `cora.find_impact`, `cora.find_affected_tests`, `cora.index_status`

- **Phase 2: Review Pipeline** (#285) — 2 new MCP tools:
  `cora.review_diff`, `cora.get_debt`

- **Phase 3: Context Enrichment** (#286) — 2 new MCP tools:
  `cora.get_project_info`, `cora.get_memory`

### Added — Cross-Product Bundle

- **Cora + Uteke bundle installer** (#235)
  - `install-bundle.sh` — single command installs both tools
  - Cross-referencing documentation across all docs

### Fixed

- **Uteke recall flag** — `--format json` → `--json` (uteke v0.0.13+ API) (#259)
- **Uteke v0.1.0 empty results parser** — handle both bare `[]` and wrapped `{"results":[]}`

## [0.5.1] - 2026-06-13

### Added

- **`cora commit`** — review staged diff + generate commit message + commit (#262)
  - HITL mode (default): interactive `[Y]es / [E]dit / [N]o` prompt
  - YOLO mode (`--yolo`): auto-commit without prompts
  - `--force`: commit even if quality gate fails
  - `--no-review`: skip review, only generate commit message
  - `--edit`: always open `$EDITOR`
  - Conventional commit format (feat/fix/refactor/perf/docs/test/chore/style/build/ci)
  - Auto-truncates subjects to 72 chars
  - Quality gate integration (block on FAIL unless `--force`)
  - Debt snapshot saved after commit
  - `chat_completion_raw()` + `chat_completion_stream_raw()` in `engine/llm.rs`
  - 22 unit tests

### Fixed

- **Uteke recall flag** — `--format json` → `--json` (uteke v0.0.13+ API change) (#259)
- **Uteke recall JSON parser** — handle both bare `[]` and wrapped `{"results":[]}` formats (uteke v0.1.0+)
- Extracted `parse_recall_json()` with 6 unit tests for format compatibility

## [0.5.0] - 2026-06-10

### Added

- **Quality Gate** — configurable threshold-based PASS/FAIL for CI enforcement (#205)
  - Global thresholds: `max_critical`, `max_major`, `max_minor`, `max_security`
  - Per-category overrides: `block`, `warn`, `ignore` actions
  - Terminal-formatted gate output with status table
  - Exit code 2 on gate failure
  - 12 unit tests covering all gate scenarios
- **Static Security Scanner** — 11 regex patterns for common vulnerabilities (#234)
  - Weak crypto (MD5/SHA1 for passwords), hardcoded secrets, SQL injection, eval(), command injection
  - Hardcoded roles, debug mode, CORS wildcard, SSL verify disabled
  - Auto-skips test files; only scans added lines
  - Findings injected into LLM prompt as additional context
- **Language-Specific Analyzers** — tailored review guidance for 6 languages (#233)
  - Dart/Flutter: widget lifecycle, state management, null safety
  - Svelte/TypeScript: reactivity, stores, SSR, type safety
  - Go: error handling, concurrency, goroutine leaks
  - Rust: ownership, lifetimes, unsafe, idioms
  - Python: type hints, async, security patterns
- **MCP Server** — expose rules and config to AI coding agents (#207)
  - JSON-RPC 2.0 over stdio transport
  - 5 tools: `list_rules`, `check_snippet`, `get_quality_gate`, `get_config`, `list_profiles`
  - `cora mcp` subcommand
  - Brace-depth stdin parsing (handles pretty-printed JSON)
  - 17 unit tests
- **Auto-chunking** — large diffs split into reviewable chunks automatically (#188)
  - `--no-auto-chunk` flag to disable
  - `src/engine/chunker.rs` module (~310 lines)
- **Tech debt metrics** — cumulative review history and trend tracking (#206)
  - `DebtSnapshot` per-review JSON snapshots with quality score (0-10)
  - `cora debt` subcommand — terminal table, `--json`, `--trend` ASCII graph, `--since`, `--branch` filters
  - Auto-save after every review (best-effort, never fails review)
  - `debt:` config section in `.cora.yaml` (history_dir, retention_days)
  - 32 unit tests
- **Uteke memory integration** — recall project patterns and learn from reviews (#232)
  - `--memory` flag — recall context from Uteke before review
  - `--learn` flag — recall + save findings after review
  - `MemoryBackend` with auto-detect, graceful degradation when Uteke not installed
  - 11 unit tests
- **Multi-platform CI docs** — Gitea/Forgejo, GitLab CI, Bitbucket Pipelines workflow examples (#225)
- **GitHub Marketplace action** — published as [`codecoradev/cora-review-action@v1`](https://github.com/marketplace/actions/cora-ai-code-review)
- **Improved review prompt** — better consistency, lower false-negative rate, explicit error handling focus area
- **Comprehensive docs/examples.md** — GitHub Actions section with setup guide, inputs reference, and provider table

### Changed

- **CI action moved to GitHub Marketplace** — workflow uses marketplace action instead of `.github/actions/cora-review/`
- **README links** — all documentation links now point to `codecora.dev` instead of relative file paths
- **CI workflows** — removed stale SvelteKit `website/` jobs, replaced with VitePress `docs/` build
- **`merge_into()` returns `Result`** — fail-fast on invalid profile config instead of silently continuing
- **Language context reuses parsed diff** — `build_language_context_from_chunks()` eliminates redundant `parse_diff()` call
- **13 stale issues closed** — migration epics, website tasks, v0.4 leftovers
- **15 stale branches deleted** — cleanup after merge

### Fixed

- **Profiles bugs** — path resolution with project root, fail-fast on invalid config, dedup merge by `id` (#238)
- **Code Scanning alert #79** — eliminated redundant `parse_diff()` call in language context injection
- **Download hardening** — 5x retry with exponential backoff, gzip validation, checksum verification for cora-code binary download in CI (#221)
- **curl hardening** — `--fail --show-error` + `set -e` guard prevents silent HTML downloads
- **Checksum enforcement** — hard fail on missing/invalid checksums (was warning-only)
- **Exact checksum match** — `awk` exact filename lookup replaces `grep` substring match

### Removed

- **SvelteKit `website/`** — 6,286 lines removed, replaced by VitePress `docs/`
- **`Website Lint` CI job** — removed from required status checks
- **Internal composite action** — `.github/actions/cora-review/` deleted, replaced by marketplace action
- **`cora-review-simple`** — unused duplicate action deleted

## [0.4.6] - 2026-06-07

### Changed

- **README redesigned** — 568 → 148 lines, professional layout with star badge, docs index table, links to docs/ for details (#162)
- **All docs updated for v0.4.5+** — changelog, getting-started, usage, roadmap, examples, installation

### Added

- **Deterministic secrets pre-scan** — 12 built-in patterns (AWS, GitHub, OpenAI, Anthropic, Groq, xAI, Slack, Stripe, Google, JWT, Private Key) run before AI review (#204)
  - Masked output: `AKIA****CDEF` (first 4 + last 4 chars shown)
  - Auto-skip test/spec/fixture/mock/example files
  - Secrets findings injected into LLM context for consistent summary
  - Fallback path blocks on critical findings even when LLM fails
- **Diff parser hardening** — hunk line count validation, broader binary detection (GIT binary patch, singular form), graceful truncated diff handling (#195 Phase 1)
- **`.agent.md` release checklist** — pre-release checklist to prevent docs drift between versions

### Fixed

- `cora config show --global` / `--project` documented in cli-reference.md (was missing)
- `cora auth login` path corrected from `config.toml` to `auth.toml` in cli-reference.md
- CI example in docs now includes CORA_BASE_URL and CORA_MODEL secrets

## [0.4.5] - 2026-06-07

### Changed

- **Config architecture redesign** — clear separation of concerns between config files (#209)
  - `~/.cora/auth.toml` now stores **only the API key** (secret)
  - `~/.cora/config.yaml` stores provider, model, base_url, and other settings (global)
  - `.cora.yaml` (project) overrides global config per-project
  - `CORA_API_KEY` env var reserved for CI use only
- **Provider info auto-migration** — if `auth.toml` still contains provider/model/base_url, automatically moved to `config.yaml` on first run
- **Deterministic rules** — `rules/` added to default exclude paths, preventing rules from matching their own source definitions (#185)

### Fixed

- **`cora config show`** — now displays the **effective resolved config** with source annotations like `[from: env CORA_PROVIDER]` instead of raw file values (#189)
- **`cora config show --global`** — new flag to show only `~/.cora/config.yaml` contents
- **`cora config show --project`** — new flag to show only `.cora.yaml` contents (mutually exclusive with `--global`)
- **`cora review` sends to wrong provider** — provider info from `auth.toml`/`config.yaml` was ignored at runtime, always defaulting to OpenAI. Now correctly reads from merged config (#209)
- **`save_provider_info` data loss** — parse failure on `config.yaml` no longer silently replaces the entire file with defaults (now returns error)
- **`cora auth login` interactive flow** — now auto-detects provider env vars (e.g. pick ZAI → detects `ZAI_API_KEY`), suggests model and base URL defaults from presets (enter to accept) (#203)
- **`cora auth login --provider zai`** — now auto-detects `ZAI_API_KEY` from environment, no need for `--api-key` flag (#184)
- **Env var override visibility** — `cora config show` now annotates which values come from env vars vs config files (#182)
- **Truncated JSON repair tests** — 12 new tests confirming `repair_truncated_json()` works correctly for all edge cases (#186)

### Added

- **`--global` / `--project` flags** on `cora config show` for scoped config inspection
- **Clap `conflicts_with`** on `--global`/`--project` — `cora config show --global --project` now rejected at CLI level
- **Interactive model/base URL prompts** — during `cora auth login`, shows preset defaults and allows override with enter-to-accept

## [0.4.4] - 2026-06-06

### Fixed

- **Spinner auto-hides in non-TTY** — `indicatif` progress spinners in `llm.rs` and `scanner.rs` now detect piped/redirected stderr and auto-hide, preventing ANSI pollution in captured output (#181)
- **Truncated JSON repair** — LLM responses cut off by max_tokens are now auto-repaired by closing unclosed strings/brackets before parse, preserving partial findings instead of failing completely (#186)

### Added

- **`--output-file <PATH>` flag** — write formatted review output to a file instead of stdout, guaranteeing capture in CI/batch pipelines (#181)

## [0.4.3] - 2026-06-06

### Fixed

- **Provider shortcut now resolves preset defaults** — bare `provider: zai` in `.cora.yaml` auto-fills `base_url` and `model` from the preset table (#183)
- **Env var override warnings** — `CORA_PROVIDER`, `CORA_MODEL`, `CORA_BASE_URL` now warn when they override config file settings (#182)
- **`config show` displays effective (resolved) config** — shows actual runtime values with `[from: env ...]` annotations when env vars override config (#189)
- **Auth file permissions auto-fix** — `~/.cora/auth.toml` permissions auto-corrected to 600 instead of just warning (#187)
- **Deterministic rules exclude own source files** — security rules no longer match against `rules/` and `tests/` directories, eliminating false positives (#185)

### Added

- **Non-interactive `cora auth login`** — `--provider`, `--api-key`, `--model`, `--base-url`, `--force` flags for scriptable setup (#184)

## [0.4.2] - 2026-06-06

### Fixed

- **Cora Review now works on fork PRs** — changed trigger from `pull_request` to `pull_request_target` so `GITHUB_TOKEN` has write access for PR comments on external contributor PRs. Explicitly checks out PR head SHA for correct diff (#178 context)

### Added

- **Top-level provider shortcuts in `.cora.yaml`** — `model:`, `base_url:`, and bare `provider:` string now accepted at top level without needing nested `provider:` section (#178, closes #176)

## [0.4.1] - 2026-06-06

### Fixed

- **Regex panic on optional hunk groups** — bare hunk headers like `@@ -1 +1 @@` (without `,count`) caused `caps[4]` index-out-of-bounds panic. Now uses `caps.get(N)` with safe fallback (#167)
- **Default max_diff_size raised to 5MB** — 50KB was too small for most real PRs (#167)
- **CI action resilience** — 3× retry on cora review failure, 600s timeout, graceful SARIF fallback when LLM API is unavailable (#174)

### Added

- **`cora init` now installs pre-commit hook** — automatically creates `.git/hooks/pre-commit` alongside `.cora.yaml`. Use `--no-hook` to skip. Falls back gracefully when not in a git repo (#176)
- **Tiered `cora auth login`** — interactive provider selection with numbered menu. Known providers (openai, anthropic, groq, ollama, zai) pre-fill base URL and model. Custom providers ask for base URL + model + key (#172)
- **Configurable CI action** — reads `.cora.yaml` from repo when present, falls back to 5MB limit when absent. Removes hardcoded `max_diff_size: 200000` (#172)
- **`on_violation` config + `--ci` mode** — hard gate for CI: `on_violation: disallow` makes cora exit non-zero on any finding. `--ci` flag enables strict non-interactive mode (#152)
- **`cora hook install/uninstall`** — explicit hook management commands (previously only via `cora init`)

### Changed

- **CI action reads `.cora.yaml`** — project config takes precedence over hardcoded fallback. `max_diff_size`, `hook.mode`, `llm.timeout` all respected in CI (#172)

## [0.4.0] - 2026-06-03

### Added

- **Deterministic rule engine** — pre-LLM regex-based rules that always report findings (no LLM dismissal). 12 built-in rules covering security (hardcoded URLs, secrets, TLS disabled, debug prints), SQL injection, TODO/FIXME, `panic!`/`unwrap` in new code, and large functions (#116)
- **Custom rules via `.cora.yaml`** — define project-specific regex rules with severity, category, exclude patterns, and glob file matching
- **Unified diff parser** — parse git diff into structured `FileChunk`/`DiffHunk`/`DiffLine` with language detection for 70+ extensions
- **File bundling engine** — smart grouping by directory and language family with configurable character/file limits. Bundle types: related, config, test, large, standalone. Token budget estimation (~4 chars/token). Defers full parallel review to v0.5 (#115)
- **Cross-file context chain** — deterministic symbol extraction (imports, function calls, type references) for 5 languages (Rust, Python, JS, Go, Java) with token-budgeted context injection into LLM prompt (#114)
- **`BundlingConfig`** — `strategy`, `max_chars_per_group`, `max_files_per_group`, `coalesce_by_directory`, `coalesce_by_language` in `.cora.yaml`
- **`ContextConfig`** — `enabled`, `max_context_tokens`, `follow_depth`, `max_symbols` in `.cora.yaml` review section
- **Default SARIF upload to GitHub Code Scanning ON** — opt-out with `upload-sarif: false` (#148)
- **SARIF tool branding** — `CodeCora` driver name (`codecoradev/cora-code`) in SARIF output (#148)

### Changed

- **Review pipeline** — rules engine runs before LLM call, context chain enriches LLM prompt with cross-file dependencies
- **LLM failure handling** — deterministic rule findings always visible even when LLM call fails

## [0.3.0] - 2026-06-03

### Added

- **Static analysis context injection** — optional clippy output injected into review prompt to reduce false positives on verified-intentional changes (#140)
- **`review.static_analysis.auto_clippy`** config — automatically run `cargo clippy` and filter output to changed files
- **`review.static_analysis.clippy_output_file`** config — read pre-computed clippy output from file
- **`cora config validate`** subcommand — validate `.cora.yaml` configuration file and report issues (#88)
- **`CoraError` enum via thiserror** — structured error types for engine layer with 17 variants (#86)

### Changed

- **Engine layer migrated from `anyhow` to `thiserror`** — structured error handling in engine, `anyhow` retained in CLI layer (#86)
- **All clippy pedantic warnings resolved** — 175 → 0 warnings across entire codebase (#84)
- **Repo URLs updated** to `codecoradev/cora-code` org (#137)
- **CI actions bumped** — `upload-artifact@v7`, Node 24 strict mode (`FORCE_JAVASCRIPT_ACTIONS_TO_NODE24`) (#142)

### Fixed

- **CI Cora Review fails on LLM API errors** — removed `|| true` suppression, added exit code + empty SARIF check (#142)
- **Match arm merge in `IssueType::from_str`** — clarified documentation (#141)

## [0.2.0] - 2026-06-02

### Added

- **`--progress` flag** — NDJSON progress events to stderr for structured CI/GUI consumers (Termul prerequisite) (#108)
- **`--max-diff-size` flag** — override `hook.max_diff_size` for large diffs from CLI (#112)
- **Output footer watermark** — Cora version stamp in terminal, SARIF, and JSON output when issues found (#106)
- **Security audit CI** — `cargo audit` via `rustsec/audit-check` for dependency CVE scanning (#85)

### Changed

- **Naive .gitignore parser → `ignore` crate** — ripgrep-grade correctness with nested .gitignore, global gitignore, and `.git/info/exclude` support (#80)
- **Blanket `#![allow(dead_code)]` removed** — targeted cleanup, 27 warnings → 0 (#79)

### Fixed

- **`REQUESTS_CA_BUNDLE` env var support** — custom CA certificates for corporate proxies, additive to built-in root certs (#74)
- **`tls_built_in_root_certs(false)` security fix** — custom CA bundle now added alongside system roots instead of replacing them (caught by Cora self-review)
- **`require_git(false)` on WalkBuilder** — gitignore rules applied even outside git repositories (#112)
- **CI `actions-rs/audit-check` → `rustsec/audit-check`** — replaced archived GitHub Action (#112)
- **Cora CI diff limit** — `CORA_CONFIG` env var with temp config for 200K char limit in CI action (#112)

## [0.1.8] - 2026-06-02

### Fixed

- **`unwrap()` → `expect()`** in ProgressStyle templates (llm.rs, scanner.rs) — clearer panic messages on template parse failure (#87)
- **Consolidated duplicate `impl Severity` blocks** into single implementation (#83)
- **`file_content_hash` returns `Option<String>`** instead of empty string on read failure — prevents infinite rescan loop on unreadable files (#77)
- **Permission errors logged in scanner** — file walk now logs permission errors at debug level instead of silently skipping (#76)
- **Auth file permission warning** — warns if `~/.cora/auth.toml` has overly permissive file permissions (Unix only) (#72)
- **SARIF upload size validation** — validates SARIF file size against GitHub's 10MB limit before upload (#82)
- **Float division for MB display** — SARIF size error now shows accurate fractional MB (was integer division truncating to 0) (#82)
- **Non-deterministic `DefaultHasher` → `sha2`** — scan cache now uses SHA-256 for deterministic hashing across Rust versions (#81)

### Added

- **`checksums-sha256.txt` in release artifacts** — release workflow generates SHA-256 checksums for all platform binaries (#109)

### Changed

- **Official CodeCora branding assets** — logo, favicon, and OG image updated from ajianaz/cora SaaS repo (#110)
- **Standalone `cora-review.yml` workflow** — CI action extracted from inline `ci.yml` job to dedicated workflow with concurrency control (#107)
- **Action v2 hardened** — all third-party actions pinned to commit SHA, checksum verification for binary downloads, env var indirection for inputs, `grep` pipefail fix, empty file guard, Node 24 strict mode compatibility (#107)

## [0.1.7] - 2026-06-01

### Added

- **Diff-hash caching** — review results cached by SHA-256 of diff + model + temperature in `~/.cache/cora/reviews/`. Cache TTL configurable via `llm.cache_ttl` (#100)
- **`--no-cache` flag** — bypass cache for fresh reviews (#100)
- **Configurable LLM parameters** — `llm.temperature` (default: 0), `llm.max_tokens` (default: 4096), `llm.timeout` (default: 120s), `llm.cache_ttl` (default: 1440 min) in `.cora.yaml` (#98 #101)
- **Git ref validation** — rejects refs containing shell metacharacters or path traversal sequences (#73)

### Fixed

- **Temperature default now 0** — eliminates non-deterministic LLM output. Same diff produces identical issues on every run (#98, #97)
- **HTTP timeout actually works** — per-request timeout via reqwest RequestBuilder (not client-level). Configurable timeout respected (#99)
- **Connection pooling** — shared reqwest::Client via LazyLock, reused across all requests (#99)
- **Cache key includes model + temperature** — config changes invalidate cache automatically (#100)
- **Silent config corruption** — malformed `.cora.yaml` now shows clear error with file path and hint (#78)
- **Composite action KeyError on API failure** — version resolution retries 3x with 5s delay, falls back to v0.1.6 with warning. Fixed in both `cora-review` and `cora-review-simple` actions (#102)

## [0.1.6] - 2026-06-01

### Added

- **Custom system prompts via config** — `review.system_prompt`, `review.system_prompt_file`, `scan.system_prompt`, `scan.system_prompt_file` fields in `.cora.yaml` (#94)
- **`response_format` config** — opt-in `json_object` response format for providers that support it, via `review.response_format: json_object` (#92)
- **File path injection into prompts** — valid diff file paths are injected into the review user prompt to reduce LLM hallucination (#93)
- **Post-parse file path filtering** — issues referencing non-existent files are filtered out after LLM response parsing (#93)
- **Enhanced default system prompts** — both review and scan prompts now include explicit anti-hallucination constraints, severity definitions, and format instructions (#95)

### Fixed

- **Path traversal in `system_prompt_file`** — arbitrary file read vulnerability. Now validates file path is within canonicalized project root (#92)
- **Symlink bypass in path traversal guard** — project root is now canonicalized to match resolved file paths

## [0.1.5] - 2026-06-01

### Fixed

- **Critical: JSON repair corrupts valid unicode escapes** — `is_valid_json_escape()` missing `'u'`, causing `\uXXXX` to be double-escaped. Now properly validates and handles incomplete `\u` sequences (#89)
- **Critical: TOML injection in `save_api_key()`** — API key written via `format!` string interpolation. Now uses `toml::Table` serialization (#69)
- **Retry prompt improvement** — retry on parse failure now includes stricter JSON format instructions (#90)
- **Temp file race condition** — SARIF upload now uses PID-suffixed temp path instead of fixed filename (#70)
- **Confusing unused `_cli_api_key` parameter** — removed from `load_config()` signature (#75)

### Security

- `save_api_key()` now uses `toml::Table::insert()` instead of string interpolation (prevents TOML injection)
- Temp SARIF file path includes process ID (prevents TOCTOU race)

## [0.1.4] - 2026-06-01

### Added

- LLM JSON repair engine (`repair_invalid_escapes`) — auto-fixes invalid escape sequences in LLM output (e.g. `\s`, `\d`) before JSON parse
- Retry mechanism in `review_diff` — if first LLM parse fails, automatically retries once
- Branding footer on "No issues found" PR comment — consistent with issues-found variant

### Fixed

- **Silent false-negative** — cora JSON parse failure previously posted "No issues found" without actual review (LLM invalid escapes)
- Hardcoded Infisical `identity-id` in `release.yml` and `deploy-website.yml` — migrated to `secrets.INFISICAL_IDENTITY_ID`
- Release workflow changelog extraction — `v` prefix mismatch (tag `v0.1.3` vs CHANGELOG `[0.1.3]`) now properly stripped
- `printf` double-escape in release workflow — `\\n` corrected to `\n`
- Stale `v0.1.2` binary download filenames in README
- Clippy `unnecessary_map_or` lint — `.map_or(false, |s| s.success())` replaced with `.is_ok_and(|s| s.success())`

### Changed

- All 3 workflows use `secrets.INFISICAL_IDENTITY_ID` (consistent with `ci.yml` pattern)
- Release workflow validates semver format before sed injection
- Branch cleanup — removed 14 stale branches

## [0.1.3] - 2026-06-01

### Added

- `cora config set --global` — write config to `~/.cora/config.yaml` instead of project `.cora.yaml`
- `cora config set base_url` — set base URL via CLI (previously only in YAML)
- Global config support (`~/.cora/config.yaml`) with priority chain: CLI flags → env vars → project → global → defaults
- Auto-migration from old `~/.cora/config.toml` to new YAML + `auth.toml` split

### Changed

- `cora config set` now writes YAML instead of TOML (compatible with config loader)
- API key storage moved from `~/.cora/config.toml` to `~/.cora/auth.toml` (0600 permissions)
- YAML serialization uses `skip_serializing_if` — no more `null` values in output

### Fixed

- **Severity comparison inverted** — `Critical` issues no longer silently pass `should_block` check (Ord ordering bug)
- Hook `mode: block` no longer exits with code 2 when "No issues found" (severity filter mismatch)
- Consistent severity logic across review, scan, and block mode paths

## [0.1.2] - 2025-05-29

### Added

- `cora init` — create `.cora.yaml` config file with provider/model selection
- `cora hook install|uninstall` — pre-commit hook management
- `cora config show|set` — configuration management
- CI composite action (`cora-review-simple`) for easy GitHub Actions integration
- Shell completions for bash, zsh, fish, and powershell
- `cora scan --incremental` with SHA256 content hash cache for fast incremental scanning
- `cora review --upload` for direct SARIF upload to GitHub Code Scanning
- `cora review --stream` for real-time review output
- `cora review --unpushed` for reviewing unpushed commits
- `cora review --base <branch>` for branch comparison
- `cora review --diff-file <path>` for reviewing external diff files
- `cora providers` command to list available LLM providers
- `cora auth login` for interactive API key storage

### Fixed

- SARIF schema compliance for GitHub Code Scanning upload
- Clippy `format_in_format_args` warnings
- Replaced deprecated `serde_yaml` with `serde_yaml_ng`
- Normalized release binary naming (`cora-{arch}-{target}-v{version}.tar.gz`)

### Changed

- Replaced deprecated dependencies
- Removed unused dependencies
- Bumped minimum Rust version to 1.85

## [0.1.1] - 2025-05-27

### Changed

- Replaced ASCII art banner with eye icon in README
- Updated README branding to cora-code

### Fixed

- CI `cargo publish` with `--allow-dirty` for Cargo.lock mismatch on tag checkout

## [0.1.0] - 2025-05-25

### Added

- **AI Code Review** — review staged changes, commit ranges, branch diffs, and full project scans
- **BYOK** — bring your own API key (OpenAI, Anthropic, Groq, Ollama, Google)
- **5 LLM Providers** — with auto-detection from installed API keys
- **Pre-commit Hooks** — `cora hook install` for automatic review on every commit
- **SARIF Output** — `--format sarif` for GitHub Code Scanning integration
- **4 Output Formats** — pretty (colored), compact, JSON, SARIF
- **Project Config** — `.cora.yaml` per-project configuration with provider, focus, rules, ignore, and hook settings
- **Environment Variables** — `CORA_API_KEY`, `CORA_MODEL`, `CORA_PROVIDER`, `CORA_BASE_URL`, `CORA_CONFIG`, `CORA_FORMAT`
- **Severity Levels** — `info`, `minor`, `major`, `critical` with configurable thresholds
- **Focus Areas** — `security`, `performance`, `bugs`, `best_practice`, `maintainability`
- **Ignore Rules** — file patterns and rule-level exclusions
- **Cross-platform** — Linux (x86_64, ARM64), macOS (Apple Silicon), Windows (x86_64)
- **MIT License** — fully open source

[Unreleased]: https://github.com/codecoradev/cora-code/compare/v0.6.1...develop
[0.6.1]: https://github.com/codecoradev/cora-code/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/codecoradev/cora-code/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/codecoradev/cora-code/compare/v0.4.6...v0.5.0
[0.4.6]: https://github.com/codecoradev/cora-code/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/codecoradev/cora-code/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/codecoradev/cora-code/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/codecoradev/cora-code/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/codecoradev/cora-code/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/codecoradev/cora-code/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/codecoradev/cora-code/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/codecoradev/cora-code/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/codecoradev/cora-code/compare/v0.1.8...v0.2.0
[0.1.8]: https://github.com/codecoradev/cora-code/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/codecoradev/cora-code/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/codecoradev/cora-code/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/codecoradev/cora-code/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/codecoradev/cora-code/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/codecoradev/cora-code/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/codecoradev/cora-code/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/codecoradev/cora-code/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/codecoradev/cora-code/releases/tag/v0.1.0