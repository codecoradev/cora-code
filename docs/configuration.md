---
title: Configuration
---

# Configuration

cora uses a layered config system with clear separation of concerns. Later sources override earlier ones.

## File Roles

| File | Contents | Used by |
|------|----------|--------|
| `~/.cora/auth.toml` | API key only (secret, chmod 600) | Local dev |
| `~/.cora/config.yaml` | Provider, model, base_url, focus, hook, output, etc. | Global default |
| `.cora.yaml` | Per-project config overrides | Project + CI |
| `CORA_API_KEY` env var | API key for CI/one-shot | CI only |

## Config Resolution Order

Settings are resolved in this order (highest priority first):

1. **CLI flags** — `--provider`, `--model`, `--base-url`, etc.
2. **Environment variables** — `CORA_PROVIDER`, `CORA_MODEL`, `CORA_BASE_URL`
3. **.cora.yaml** — Project root config file
4. **~/.cora/config.yaml** — Global config
5. **Auto-detect** — Provider-specific env vars (`OPENAI_API_KEY`, `ZAI_API_KEY`, etc.)
6. **Built-in defaults** — Sensible defaults for all settings

After all sources are merged, **config values are validated at load time**: out-of-range values (e.g. `temperature: 5`), unsupported formats (`output.format: prety`), and misspelled keys (`quailty_gate`, `temprature`) fail loudly with a clear message instead of being silently ignored. This applies to `temperature` (0.0–2.0), `max_tokens`/`timeout` (≥1), `max_tokens_param`, `response_format`, `output.format`, `hook.mode`/`on_violation`/`min_severity`, `provider.base_url`, and profile `weight`/`action`/`tone`/`detail_level`.

### API Key Resolution

1. `--api-key` flag (one-shot)
2. `CORA_API_KEY` env var (CI)
3. `~/.cora/auth.toml` (local dev)
4. Provider-specific env vars (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.)

## .cora.yaml Example

Create this file in your project root. Run `cora init` to generate it.

```yaml
# cora project config
provider:
  provider: openai
  model: gpt-4o
  base_url: https://api.openai.com/v1

llm:
  temperature: 0
  max_tokens: 4096
  max_tokens_param: auto       # auto | max_tokens | max_output_tokens | max_completion_tokens
  timeout: 120
  cache_ttl: 1440

review:
  system_prompt: "You are a senior code reviewer."
  # system_prompt_file: ./review-prompt.md
  response_format: json_object
  static_analysis:
    auto_clippy: false       # auto-run `cargo clippy` (Rust only)
    clippy_output_file: ""   # or read clippy output from file

focus: security, performance, bugs

hook:
  mode: warn
  min_severity: major
  max_diff_size: 51200
  on_violation: warn           # warn | disallow (blocks commit if violations found)

ignore:
  files:
    - "vendor/**"
    - "*.generated.ts"
  rules: []                    # rule IDs to skip (e.g. ["no-unwrap", "sql-injection"])

output:
  format: pretty               # pretty | json | compact | sarif
  color: true                  # ANSI colors in terminal output

quality_gate:
  enabled: true
  thresholds:
    max_critical: 0
    max_security: 0

bundling:
  max_chars_per_group: 60000
  max_files_per_group: 20
  strategy: smart            # smart | flat
  coalesce_by_directory: true
  coalesce_by_language: true

analysis:
  entry_point_patterns:
    - "*Handler"
    - "resolve_*"

brain:
  embedding: auto           # auto | hashing | pretrained

profile: clean-code        # security-first | performance | clean-code | beginner-friendly | minimal | rust-strict | typescript-strict | go-pragmatic
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `CORA_API_KEY` | API key for CI (overrides auth.toml) |
| `CORA_PROVIDER` | Active provider (openai, anthropic, groq, ollama, zai) |
| `CORA_MODEL` | Model name override |
| `CORA_BASE_URL` | Custom API base URL |
| `CORA_CONFIG` | Path to config file |
| `CORA_FORMAT` | Output format (pretty, json, compact, sarif) |
| `CORA_NO_COLOR` | Disable colored output |
| `CORA_NO_CACHE` | Skip diff-hash review cache (same as `--no-cache`) |
| `GITHUB_TOKEN` | GitHub token for SARIF upload |
| `GITHUB_REPOSITORY` | GitHub repo for SARIF upload |
| `GITHUB_REF` | GitHub ref for SARIF upload |

## Provider-Specific Env Vars

Each provider has its own API key variable. cora checks these for auto-detection.

```bash
# OpenAI
OPENAI_API_KEY=sk-...
OPENAI_BASE_URL=https://api.openai.com/v1

# Anthropic
ANTHROPIC_API_KEY=sk-ant-...

# Groq
GROQ_API_KEY=gsk_...

# Ollama (local, no key needed)
OLLAMA_HOST=http://localhost:11434
# Optional: OLLAMA_API_KEY if your Ollama instance requires auth
OLLAMA_API_KEY=...

# Z.AI
ZAI_API_KEY=...
```

## Diff-Hash Caching

cora caches review results by diff hash in `~/.cache/cora/reviews/`. If you re-review the same diff, the cached result is returned instantly.

| Setting | Description |
|---------|-------------|
| `llm.cache_ttl` | TTL in minutes (default: 1440 / 24h) |
| `--no-cache` or `CORA_NO_CACHE=1` | Bypass cache |

## Custom System Prompts

Override the default system prompt for `review` or `scan` commands to match your project's coding standards and review criteria.

```yaml
review:
  system_prompt: "Focus on Rust idioms and error handling."
  # Or load from a file:
  system_prompt_file: ./prompts/review.md

scan:
  system_prompt: "Check for OWASP Top 10 vulnerabilities."
  system_prompt_file: ./prompts/scan.md
```

If both `system_prompt` and `system_prompt_file` are set, the file takes precedence.

## Response Format (JSON Mode)

Opt into structured JSON output from the LLM by setting `review.response_format` to `json_object`. This instructs the LLM to return valid JSON, enabling machine-readable parsing and pipeline integration.

```yaml
review:
  response_format: json_object
```

Requires provider support for structured output. Works with OpenAI, Anthropic, and compatible APIs.

## Anti-Hallucination

cora uses two mechanisms to prevent the LLM from fabricating findings:

- **File path injection** — Actual file paths are embedded in the prompt, anchoring the LLM to real files in the diff.
- **Post-parse filtering** — After parsing, any reported file paths or line numbers that don't exist in the actual diff are discarded.

## Cross-File Review Context

To review a change accurately, cora injects **cross-file context** alongside the diff — it doesn't review the diff in isolation. This is deterministic (no extra LLM calls) and bounded by a token budget so cost stays predictable.

Two axes are resolved:

- **Outbound** — what the changed code *calls/imports* (function/type definitions the diff references).
- **Inbound (blast radius)** — *who calls the changed code*. If a PR modifies a function signature or type, its call-sites across the repo are surfaced so breaking changes can be flagged.

When the budget can't fit a full definition, a thin **signature slice** is injected instead of skipping it, so more symbols fit under the same budget.

```yaml
review:
  context_chain:
    enabled: true               # master switch for cross-file context
    max_context_tokens: 5000    # budget (~4 chars/token) for injected context
    follow_depth: 1             # outbound resolution depth (1 = direct refs only)
    include_tests: true         # resolve test files via naming convention
    include_callers: true       # resolve callers of changed code (blast radius)
    use_brain: true             # enrich prompt with symbol-index intelligence
    impact_depth: 2             # blast-radius traversal depth (2 = callers of callers)
    prefer_index: true          # prefer symbol index (FTS5 + call graph) over regex
```

| Field | Default | Notes |
|-------|---------|-------|
| `enabled` | `true` | Disable to review the diff only. |
| `max_context_tokens` | `5000` | Approx. 20 KB of code injected. |
| `follow_depth` | `1` | Outbound recursion depth (`1` = direct references). |
| `include_tests` | `true` | Map changed source to its test files. |
| `include_callers` | `true` | Inbound caller resolution. When the symbol index is unavailable (regex fallback): gitignore-aware file scan bounded to ≤400 files and ≤3 call-sites per symbol. When the index is available (default path, `prefer_index: true`): up to 20 call-sites per symbol, filtered by `ignore.files` patterns. |
| `use_brain` | `true` | Enrich prompt with symbol-index intelligence (impact analysis, affected tests, semantic search). Only active when `cora index` has been run. |
| `impact_depth` | `2` | Blast-radius traversal depth. See [Impact depth guidance](#impact-depth-guidance) below. |
| `prefer_index` | `true` | Prefer symbol index (FTS5 + call graph) over regex scanning for outbound resolution. |

### Impact Depth Guidance

`impact_depth` controls how many levels **up** the call graph the blast-radius traversal follows:

- `1` — direct callers only. Cheapest, misses indirect breakage.
- `2` — callers of callers. **Recommended default** — covers the common layered case (handler → service → helper).
- `3` — deep blast radius for strongly layered codebases (handler → service → repository → helper). Higher token cost.
- `4+` — rarely worth it. The traversal is BFS with cycle protection, so it is always safe, but on real codebases the caller count grows quickly with depth: heavily-used utility functions and entry points turn into hubs with hundreds of callers, and the injected context fills with noise rather than signal.

**Why not "unlimited"?** Setting a very large depth is technically valid — traversal stops on its own once every reachable caller is visited — but on any non-trivial repo it surfaces the entire transitive caller closure. The prompt budget (`max_context_tokens`) then truncates the output arbitrarily, so you pay full traversal cost for context the LLM never sees.

**How to choose:**

- Small / flat repo → leave at `2`.
- Layered repo (handler/service/repository split) where bugs manifest several layers from the root cause → `3` for that repo's `.cora.yaml`.
- Want *precision* rather than *reach* → keep `2` and rely on `cora impact` interactively with `--depth` to explore specific symbols on demand.

> **Note:** `impact_depth` only governs the *automatic* blast-radius context injected during `cora review`. The interactive `cora impact` / `cora trace` commands default to `--depth 3` and are unaffected by this setting.

## Quality Gate

Quality gate evaluates review findings against configurable thresholds to produce a **PASS/FAIL** result. This is useful for CI enforcement — block merges when code quality drops below your standards.

```yaml
quality_gate:
  enabled: true

  # Global thresholds — any exceeded = FAIL
  thresholds:
    max_critical: 0        # 0 critical issues allowed
    max_major: 3           # max 3 major issues (disabled by default)
    max_minor: 10          # max 10 minor issues (disabled by default)
    max_security: 0        # 0 security findings allowed

  # Per-category overrides
  categories:
    security:
      action: block        # block = any finding → CI fail
      max_findings: 0
    performance:
      action: warn         # warn = comment only, don't fail CI
      max_findings: 5
    bug_risk:
      action: block
      max_findings: 3
    style:
      action: ignore       # skip entirely — don't count toward gate
```

### How It Works

1. After review, findings are counted by severity and category
2. Each threshold is checked against actual counts
3. Category actions determine enforcement:
   - **block** — exceed threshold = gate FAIL (exit code 2)
   - **warn** — report but don't fail gate
   - **ignore** — skip entirely

   Actions are validated enums — case-insensitive (`block`, `Block`, `BLOCK` all work), and an unknown value (e.g. `blok`) fails at config load instead of silently becoming blocking. A disabled gate (`enabled: false`) never fails.
4. Overall gate status: **PASSED** or **FAILED**

### CLI Output

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  QUALITY GATE RESULT
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Status:   ❌ FAILED
  Findings: 2 critical, 1 major, 4 minor, 0 info

  Threshold Checks:
  ❌ max_critical          → 2 found   ❌ EXCEEDED
  ✅ max_major             → 1 found   ✅ OK
  ✅ max_security          → 0 found   ✅ OK
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### Exit Codes

| Code | Meaning |
|------|----------|
| 0 | Gate passed, no issues |
| 2 | Gate failed (threshold exceeded) |

### Default Behavior

When `quality_gate.enabled` is `false` (default), quality gate is skipped. The existing `--ci` flag and `hook.on_violation` settings continue to work as before.

## Debt Tracking

cora tracks review findings over time and reports quality trends. After each review, a lightweight JSON snapshot is saved to `.cora/history/`. Use `cora debt` to view the aggregated report.

```yaml
debt:
  enabled: true              # enable auto-save of review snapshots
  history_dir: .cora/history # directory for snapshot files
  retention_days: 90         # auto-cleanup old snapshots
```

### CLI Usage

```bash
cora debt                    # show debt report table
cora debt --json             # machine-readable JSON
cora debt --trend            # quality score trend graph
cora debt --badge            # shields.io badge JSON
cora debt --estimate         # show estimated fix time
cora debt --since v0.4.5     # filter by git tag or date
```

### Quality Score

Ranges from 0–10 (10 = no issues). Penalties per finding:

| Severity | Penalty |
|----------|--------|
| Critical | -2.0 |
| Major | -1.0 |
| Minor | -0.3 |
| Info | -0.1 |

### Badge Integration

Use `cora debt --badge` output as a shields.io endpoint:

```markdown
[![Quality](https://img.shields.io/endpoint?url=https://example.com/cora-badge.json)]()
```

## Secrets Pre-Scan

cora runs a deterministic secrets scan before the AI review. 12 built-in patterns detect leaked credentials:

| Pattern | Severity |
|---------|----------|
| AWS Access Key (`AKIA...`) | Critical |
| GitHub Token (`ghp_`/`gho_`/`ghu_`) | Critical |
| OpenAI API Key (`sk-`/`sk-proj-`) | Critical |
| Anthropic API Key (`sk-ant-`) | Critical |
| Private Key Block | Critical |
| JWT Token | Major |
| And more (Groq, xAI, Slack, Stripe, Google) | Varies |

Secrets are automatically **masked** in output (e.g. `AKIA****CDEF`). Test/spec/fixture files are auto-skipped.

## Static Security Scanner

cora runs a static security scan on added lines before the AI review. 11 built-in patterns detect common vulnerabilities:

| Pattern | Category | Severity |
|---------|----------|----------|
| MD5 used for password hashing | Weak crypto | Major |
| SHA-1 used for password hashing | Weak crypto | Major |
| Weak hash algorithm (MD5/SHA1) | Weak crypto | Minor |
| Hardcoded password or secret | Hardcoded secret | Critical |
| SQL injection via string concatenation | Injection | Critical |
| eval() with dynamic input | Injection | Critical |
| Command injection via exec/system | Injection | Critical |
| Hardcoded role or permission check | Auth | Major |
| Debug mode enabled | Config | Major |
| CORS wildcard allows all origins | Config | Major |
| SSL certificate verification disabled | Crypto | Critical |

Test files are automatically skipped. Findings are injected into the LLM prompt as additional context.

## Language-Specific Analyzers

cora detects the languages in your diff and injects tailored review guidance:

| Language | Guidance |
|----------|----------|
| **Dart / Flutter** | Widget lifecycle, state management, async patterns, null safety |
| **Svelte / TypeScript** | Reactivity, store patterns, SSR considerations, type safety |
| **Go** | Error handling, concurrency, goroutine leaks, interface design |
| **Rust** | Ownership, lifetimes, error handling, unsafe usage, idioms |
| **Python** | Type hints, async patterns, security (pickle/eval), common pitfalls |

No configuration needed — language context is auto-detected from file extensions in the diff.

## Quality Profiles

cora includes built-in quality profiles for different review focus:

| Profile | Description |
|---------|------------|
| `security-first` | Strict security focus — zero tolerance for vulnerabilities |
| `performance` | Focus on speed, memory, and allocation patterns — best for hot-path code |
| `clean-code` | *(default)* Broad quality — readability, naming, complexity — best for team projects |
| `beginner-friendly` | Gentle review — focus on common mistakes and learning opportunities |
| `minimal` | Only critical + security — best for quick PRs and hotfixes |
| `rust-strict` | Rust-specific: unsafe, unwrap, panic, lifetime, error handling, idiomatic patterns |
| `typescript-strict` | TypeScript-specific: any types, null safety, proper typing, async patterns |
| `go-pragmatic` | Go-specific: error handling, goroutine safety, interface design, idiomatic Go |

Run `cora profile list` to see all profiles. Cora auto-detects the best profile based on your project's primary language (Rust → `rust-strict`, Go → `go-pragmatic`, others → `clean-code`).

Set in `.cora.yaml`:

```yaml
profile: security-first
```

## Custom Rule Engine

Write your own regex-based rules in `.cora.yaml`:

```yaml
rules:
  - id: no-unwrap
    pattern: "\\.unwrap\\(\\)"
    severity: minor
    message: "Avoid unwrap() in production code — use proper error handling"
    languages: ["rust"]
    exclude: ["tests/**"]

  - id: no-console-log
    pattern: "console\\.log\\("
    severity: minor
    message: "Remove console.log before merging"
    languages: ["typescript", "javascript"]
```

Rules run during the deterministic pre-scan phase (no LLM needed).

### Index Scanner Configuration

Control how index-based scanners (unused imports, dead code, breaking changes) behave — including file skip patterns to reduce false positives on bundler entry points and config files.

```yaml
rules_engine:
  enabled: true
  max_findings: 5
  index_skip_files:
    - "*.config.ts"
    - "vite.config.*"
    - "webpack.config.*"
    - "tailwind.config.*"
    - "next.config.*"
    - "src/main.ts"
    - "src/index.tsx"
    - "src/app.tsx"
    - "build.rs"
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable/disable the rule engine |
| `max_findings` | `int` | `5` | Max findings per scan |
| `index_skip_files` | `[string]` | *(see below)* | Glob patterns for files to skip during index scanning |

Default `index_skip_files` patterns (bundled with cora):

- `*.config.ts`, `*.config.js`, `*.config.mjs` — bundler/build config files
- `vite.config.*`, `next.config.*`, `tailwind.config.*` — framework config files
- `src/main.ts`, `src/index.tsx`, `src/app.tsx`, `src/main.rs`, `src/lib.rs` — app entry points
- `build.rs` — Rust build scripts

> **Why skip these?** Entry-point files and bundler configs often import symbols that are consumed by the bundler/compiler, not by other source code. Index scanners detect these as "unused" because there are no code-level references. Skipping them eliminates the most common source of false positives.

Glob patterns support: exact match (`main.rs`), wildcard suffix (`*.config.ts`), wildcard prefix (`vite.*`), and any-directory (`**/main.ts`).

### Brain Embedding Backend

Control which embedding backend Brain Mode uses for vector search. Selectable at runtime — no recompilation needed to switch.

```yaml
brain:
  embedding: auto   # auto | hashing | pretrained
```

| Value | Dimensions | Description |
|-------|------------|-------------|
| `auto` (default) | 256 or 768 | Best available — uses `pretrained` if compiled with `pretrained-embed` feature, otherwise `hashing` |
| `hashing` | 256 | Force zero-dependency bag-of-tokens hashing. Always available, no model download |
| `pretrained` | 768 | Force nomic-embed-code distilled embeddings. Requires `pretrained-embed` feature at build time |

> **⚠️ Cannot mix dimensions.** 256d and 768d vectors cannot coexist in the same usearch index. After changing this setting, run `cora index --rebuild` to regenerate embeddings with the new backend.

> **Note:** If you select `pretrained` but cora was built without the `pretrained-embed` feature, it falls back to `hashing` with a warning.

## Ignore Files

Exclude files or directories from **all** cora operations — review, scan, and indexing. This is the broadest exclusion mechanism.

```yaml
ignore:
  files:
    - "vendor/**"
    - "*.min.js"
    - "**/generated/**"
    - "*.lock"
```

| Pattern | Matches |
|---------|---------|
| `src/main.ts` | Exact path — only `src/main.ts` |
| `*.config.ts` | Suffix wildcard — any file ending in `.config.ts` |
| `vite.config.*` | Prefix wildcard — `vite.config.js`, `vite.config.ts` |
| `**/main.ts` | Any-dir name — `src/main.ts`, `app/main.ts`, `a/b/main.ts` |
| `**/phaser/**` | Any-dir wildcard — any path containing a `phaser/` directory |
| `src/engine/**` | Prefix-dir — everything under `src/engine/` |
| `**/*.test.ts` | Double wildcard ext — any `.test.ts` file anywhere |

**Auto-skipped by default** (gitignore-aware): `node_modules/`, `target/`, `.git/`, `dist/`, `build/`.

> **`ignore.files` vs `rules_engine.index_skip_files`:** `ignore.files` excludes files from **everything** (review, scan, index). `rules_engine.index_skip_files` excludes files from **index scanners only** (dead code, unused imports) — they're still reviewed by the LLM.

## Static Analysis

Run language-specific static analysis tools automatically during review and feed their output to the LLM for better findings.

```yaml
review:
  static_analysis:
    auto_clippy: false       # auto-run `cargo clippy` (Rust only)
    clippy_output_file: ""   # or read clippy output from a file
```

| Field | Default | Description |
|-------|---------|-------------|
| `auto_clippy` | `false` | Auto-run `cargo clippy --message-format=json` and inject warnings into review context (Rust projects only) |
| `clippy_output_file` | `""` | Read clippy JSON output from a file instead of running clippy (useful for CI where clippy runs separately) |

## Bundling

> **Deprecation notice:** as of the current release, `cora scan` does **not** read this section — batch sizing is governed by `--batch-files` (default 20) and an internal ~60,000-character batch budget. These keys are parsed but have no effect on `cora scan`. They are kept for backward compatibility; do not rely on them.

```yaml
bundling:
  max_chars_per_group: 60000  # max source characters per LLM batch
  max_files_per_group: 20      # max files per batch
  strategy: smart              # grouping strategy: smart | flat
  coalesce_by_directory: true  # merge small batches from the same directory
  coalesce_by_language: true   # merge small batches with the same language
```

| Field | Default | Description |
|-------|---------|-------------|
| `max_chars_per_group` | `60000` | Soft limit on source characters per batch |
| `max_files_per_group` | `20` | Max files per batch before splitting |
| `strategy` | `smart` | Grouping strategy: `smart` (coalesce by directory + language within limits) or `flat` (first-fit by character count, legacy) |
| `coalesce_by_directory` | `true` | Merge small batches from the same directory into one LLM call |
| `coalesce_by_language` | `true` | Merge small batches with the same primary language |

## Analysis

Configure entry-point symbol patterns for architecture and call-graph analysis. Entry points are treated as roots when tracing execution paths and detecting dead code.

```yaml
analysis:
  entry_point_patterns:
    - "*Handler"
    - "resolve_*"
    - "*Middleware"
    - "main"
```

| Field | Default | Description |
|-------|---------|-------------|
| `entry_point_patterns` | `[]` | Glob patterns identifying entry-point symbols (used by `dead-code`, `trace`, and `arch` commands to avoid false positives on intentionally-unreachable functions) |

## Tech Debt Tracker

cora tracks review history and calculates tech debt metrics over time.

### Config

```yaml
debt:
  enabled: true           # default: true
  history_dir: .cora/history  # snapshot storage
  retention_days: 90      # auto-cleanup old snapshots
```

### CLI

```bash
cora debt                    # Show debt report table
cora debt --json             # Machine-readable JSON output
cora debt --trend            # ASCII quality score graph
cora debt --since 2026-06-01 # Filter by date
cora debt --since v0.4.5     # Filter by git tag
cora debt --branch develop   # Filter by branch
```

Snapshots are auto-saved after every review (best-effort, never fails the review).

## MCP Server

cora includes a built-in MCP (Model Context Protocol) server that exposes rules and config to AI coding agents like Claude Code, Cursor, Copilot, and Windsurf.

### Start the server

```bash
cora mcp      # Start MCP server
cora serve    # Start MCP server + auto-reindex on startup (ensures fresh index)
```

### Available tools

| Tool | Description |
|------|-------------|
| `cora.list_rules` | List all rules, security patterns, and secret patterns |
| `cora.check_snippet` | Check a code snippet against deterministic scanners (no LLM) |
| `cora.get_quality_gate` | Get quality gate config and thresholds |
| `cora.get_config` | Get effective project config (no secrets exposed) |
| `cora.list_profiles` | List all quality profiles |
| `cora.search_symbols` | Search the symbol index (requires `cora index`) |
| `cora.find_callers` | Find all callers of a symbol (reverse call graph) |
| `cora.find_impact` | Analyze blast radius of changing a symbol |
| `cora.find_affected_tests` | Find test files affected by changed source files |
| `cora.index_status` | Check if a symbol index exists and get statistics |
| `cora.review_diff` | Review a git diff using cora's full pipeline (makes LLM call) |
| `cora.get_debt` | Get tech debt report from review history |
| `cora.get_project_info` | Get project context (repo, branch, cora version, index status) |
| `cora.get_memory` | Recall project patterns from Uteke (requires `uteke` CLI) |
| `cora.brain_search` | Hybrid code search: FTS5 + vector + graph → RRF fusion |
| `cora.install` | Detect installed AI agents and configure cora as MCP server |
| `cora.dead_code` | Find potentially dead code (functions with no callers) |
| `cora.query` | Query the code graph with simple patterns (e.g. `main -> *`) |

### Configure in Claude Code

Add to your project's `.claude/settings.json`:

```json
{
  "mcpServers": {
    "cora": {
      "command": "cora",
      "args": ["mcp"]
    }
  }
}
```

### Configure in Cursor

Add to `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "cora": {
      "command": "cora",
      "args": ["mcp"]
    }
  }
}
```

The MCP server communicates via JSON-RPC 2.0 over stdio — no HTTP server needed.
