---
title: Roadmap
---

# Roadmap

Demand-gated — we build what people actually need. Track progress on [GitHub Issues](https://github.com/codecoradev/cora-code/issues).

## v0.1.5 — Initial Release

- [#90](https://github.com/codecoradev/cora-code/issues/90) Basic diff review with OpenAI — ✓ Done
- [#89](https://github.com/codecoradev/cora-code/issues/89) JSON response repair & unicode handling — ✓ Done
- [#90](https://github.com/codecoradev/cora-code/issues/90) CLI interface with review command — ✓ Done

## v0.1.6 — Custom Prompts & Path Injection

- [#95](https://github.com/codecoradev/cora-code/issues/95) Enhanced default system prompts — ✓ Done
- [#94](https://github.com/codecoradev/cora-code/issues/94) Custom system prompt via .cora.yaml config — ✓ Done
- [#93](https://github.com/codecoradev/cora-code/issues/93) Inject valid file paths into system prompt — ✓ Done
- [#92](https://github.com/codecoradev/cora-code/issues/92) JSON object response format (opt-in) — ✓ Done

## v0.1.7 — Deterministic & Reliable

- [#98](https://github.com/codecoradev/cora-code/issues/98) Deterministic reviews — temperature=0 — ✓ Done
- [#97](https://github.com/codecoradev/cora-code/issues/97) Non-deterministic output bug fix — ✓ Done
- [#99](https://github.com/codecoradev/cora-code/issues/99) HTTP timeout + connection pooling — ✓ Done
- [#100](https://github.com/codecoradev/cora-code/issues/100) Diff-hash caching for repeat reviews — ✓ Done
- [#101](https://github.com/codecoradev/cora-code/issues/101) Configurable max_tokens — ✓ Done

## v0.2.0 — Multi-Provider & SARIF

- [#106](https://github.com/codecoradev/cora-code/issues/106) BYOK — Anthropic, Groq, Ollama support — ✓ Done
- [#106](https://github.com/codecoradev/cora-code/issues/106) SARIF output format — ✓ Done
- [#106](https://github.com/codecoradev/cora-code/issues/106) Branch review mode — ✓ Done
- [#106](https://github.com/codecoradev/cora-code/issues/106) Output footer watermark — ✓ Done

## v0.3 — Progress & CI Hardening

- [#140](https://github.com/codecoradev/cora-code/issues/140) Static analysis context injection (reduce false positives) — ✓ Done
- [#108](https://github.com/codecoradev/cora-code/issues/108) --progress flag for machine-readable output — ✓ Done
- [#102](https://github.com/codecoradev/cora-code/issues/102) Composite action crash fix (KeyError) — ✓ Done
- [#88](https://github.com/codecoradev/cora-code/issues/88) Config validate command — ✓ Done

## v0.4 — Deterministic Engine Pipeline

- [#116](https://github.com/codecoradev/cora-code/issues/116) Deterministic rule engine — 12 built-in rules — ✓ Done
- [#115](https://github.com/codecoradev/cora-code/issues/115) File bundling — parallel per-bundle review — ✓ Done
- [#114](https://github.com/codecoradev/cora-code/issues/114) AST-based cross-file dependency extraction — ✓ Done
- [#159](https://github.com/codecoradev/cora-code/issues/159) Hunk header regex panic fix + 5MB diff support — ✓ Done

## v0.4.5 — Config Architecture

- [#209](https://github.com/codecoradev/cora-code/issues/209) Config redesign — auth.toml for secrets, config.yaml for settings — ✓ Done
- [#203](https://github.com/codecoradev/cora-code/issues/203) Auth login auto-detect provider env vars — ✓ Done
- [#189](https://github.com/codecoradev/cora-code/issues/189) `cora config show` effective resolved config — ✓ Done
- [#182](https://github.com/codecoradev/cora-code/issues/182) Env var override visibility — ✓ Done
- [#185](https://github.com/codecoradev/cora-code/issues/185) Deterministic rules exclude `rules/` — ✓ Done
- [#186](https://github.com/codecoradev/cora-code/issues/186) Truncated JSON repair tests — ✓ Done

## v0.4.6 — Polish & Docs

- [#162](https://github.com/codecoradev/cora-code/issues/162) README overhaul — market-facing copy — ✓ Done
- [#204](https://github.com/codecoradev/cora-code/issues/204) Deterministic secrets pre-scan — ✓ Done
- [#195](https://github.com/codecoradev/cora-code/issues/195) Diff parser hardening Phase 1 — ✓ Done

## v0.5 — Agent & Quality

- [#205](https://github.com/codecoradev/cora-code/issues/205) Quality gate — CI pass/fail thresholds — ✓ Done
- [#234](https://github.com/codecoradev/cora-code/issues/234) Static security scanner — 11 patterns — ✓ Done
- [#233](https://github.com/codecoradev/cora-code/issues/233) Language-specific analyzers (Dart, Svelte, TS, Go, Rust, Python) — ✓ Done
- [#207](https://github.com/codecoradev/cora-code/issues/207) MCP server — expose rules to AI agents — ✓ Done
- [#238](https://github.com/codecoradev/cora-code/issues/238) Quality profiles bug fix — path resolution, fail-fast, dedup — ✓ Done
- [#188](https://github.com/codecoradev/cora-code/issues/188) Auto-chunking for large diffs — ✓ Done
- [#206](https://github.com/codecoradev/cora-code/issues/206) Tech debt metrics — review history — ✓ Done
- [#232](https://github.com/codecoradev/cora-code/issues/232) Uteke memory integration — recall + learn — ✓ Done
- [#262](https://github.com/codecoradev/cora-code/issues/262) `cora commit` — review + auto commit message + quality gate — ✓ Done

## v0.6 — Code Intelligence

The foundation layer for structural code understanding — persistent symbol index, semantic search, and deep Uteke integration.

- [#264](https://github.com/codecoradev/cora-code/issues/264) `cora index` — Symbol index & SQLite persistence — ✓ Done
- [#265](https://github.com/codecoradev/cora-code/issues/265) `cora explore` — Structural + semantic code search — ✓ Done
- [#235](https://github.com/codecoradev/cora-code/issues/235) Cora + Uteke cross-product integration bundle — ✓ Done
- [#284](https://github.com/codecoradev/cora-code/issues/284) MCP Phase 1 — Code Intelligence tools (5 tools) — ✓ Done
- [#285](https://github.com/codecoradev/cora-code/issues/285) MCP Phase 2 — Review + Debt tools (2 tools) — ✓ Done
- [#286](https://github.com/codecoradev/cora-code/issues/286) MCP Phase 3 — Context enrichment tools (2 tools) — ✓ Done

### Also in v0.6

- [#47](https://github.com/codecoradev/cora-code/issues/47) GitHub Marketplace action — ✓ Done
- [#196](https://github.com/codecoradev/cora-code/issues/196) VitePress docs site — ✓ Done
- [#161](https://github.com/codecoradev/cora-code/issues/161) `cora gain` — local stats + viral sharing — ✓ Done
- [#160](https://github.com/codecoradev/cora-code/issues/160) Landing page redesign — ✓ Done

## v0.7 — Multi-Language & Code Graph

Query layer built on top of the v0.6 index — call graph traversal, test impact analysis, broader language support, and real-time sync.

- [#266](https://github.com/codecoradev/cora-code/issues/266) `cora callers` / `cora impact` — Call graph query commands — ✓ Done
- [#267](https://github.com/codecoradev/cora-code/issues/267) `cora affected` — Find tests affected by changes — ✓ Done
- [#268](https://github.com/codecoradev/cora-code/issues/268) Language expansion — 6 → 15+ language support — ✓ Done
- [#269](https://github.com/codecoradev/cora-code/issues/269) Auto-sync file watcher daemon — ✓ Done

## v0.8 — Brain Mode & Tree-sitter Expansion

Hybrid semantic search and deep language support via AST parsing.

- [Phase 1](https://github.com/codecoradev/cora-code/pull/354) — Static token embedding engine (256d) — ✓ Done
- [Phase 2](https://github.com/codecoradev/cora-code/pull/356) — Tree-sitter AST + schema v3 edges table — ✓ Done
- [Phase 2C](https://github.com/codecoradev/cora-code/pull/358) — `cora trace` + `cora arch` — ✓ Done
- [Phase 3](https://github.com/codecoradev/cora-code/pull/362) — Brain Mode hybrid search (usearch + RRF) — ✓ Done
- [#374](https://github.com/codecoradev/cora-code/pull/374) Dart symbol indexing — ✓ Done
- [#375](https://github.com/codecoradev/cora-code/pull/375) Svelte symbol indexing — ✓ Done
- [#376](https://github.com/codecoradev/cora-code/pull/376) Tree-sitter expansion: 4 → 12 languages (Java, C, C++, C#, Ruby, PHP, Scala, JS) — ✓ Done
- [#338](https://github.com/codecoradev/cora-code/pull/338) Renamed `cora-cli` → `cora-code` — ✓ Done
- [#369](https://github.com/codecoradev/cora-code/pull/369) Security scanner false positive suppression — ✓ Done
- [#355](https://github.com/codecoradev/cora-code/pull/355) Global index database (`~/.codecora/cora-code/graph.db`) — ✓ Done

## Future — What's Next

### Other

- [#117](https://github.com/codecoradev/cora-code/issues/117) Lightweight agent follow-up — 1 capped tool-call — → Planned
- [#132](https://github.com/codecoradev/cora-code/issues/132) GitHub App backend MVP in Rust (Axum) — → Planned
