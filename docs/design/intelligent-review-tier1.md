# Design: Intelligent Review — Wire Index to Review Pipeline

## Problem

Cora v0.10.0 punya **dua sistem terpisah** yang tidak saling terhubung:

1. **Context Chain Resolver** (`engine/context/resolver.rs`) — regex-based file scan untuk cross-file dependency. Dipakai oleh `cora review`.
2. **Brain Intelligence** (`index/graph.rs`, `index/brain.rs`) — symbol index, call graph, FTS5+vector+graph RRF search. **Hanya** dipakai oleh MCP tools (`cora brain_search`, `cora find_callers`, dll).

Review pipeline saat ini **tidak memanfaatkan symbol index**. Caller resolution di resolver pakai regex grep yang scan seluruh filesystem — lebih lambat, kurang akurat, dan dibatasi MAX 50 files.

## Goal

Membuat `cora review` otomatis memanfaatkan symbol index (brain intelligence) **kalau index tersedia**, dengan fallback ke regex-based resolver **kalau tidak ada index**. Zero breaking change.

## Scope (Tier 1 — Minimum Viable Intelligence)

### 1.1: Index-Aware Caller Resolution

**File:** `src/engine/context/resolver.rs` → `resolve_callers()`

**Current behavior:**
```
resolve_callers() → Walk project files → regex grep per file → max 50 files
```

**Proposed behavior:**
```
resolve_callers() → cek index available?
  ├─ YES → graph::find_callers(conn, project_id, symbol_name, limit)
  │        ✅ O(1) SQL query, exact match, unlimited scope
  └─ NO  → existing regex scan (fallback, unchanged)
```

**Implementation:**
- Import `crate::index::{open_global_index, ensure_project}` di resolver.rs
- Di awal `resolve_callers()`, cek apakah `open_global_index()` sukses
- Kalau sukses, loop `defs` dan query `graph::find_callers()` per symbol
- Convert `CallerResult` → `ContextEntry` (reuse existing `build_entry` logic)
- Kalau index gagal/error, fallthrough ke existing regex scan
- Log: `debug!("using index for caller resolution ({} symbols)", defs.len())`

**Token budget:** Caller results dari index masuk ke token budget yang sama dengan existing context chain (`max_context_tokens`). Tidak ada budget tambahan.

### 1.2: Impact Analysis Injection

**File:** `src/engine/review.rs` — setelah context chain build (line ~197)

**New behavior:**
```
context_chain built → cek index available?
  ├─ YES → untuk setiap defined symbol:
  │        graph::impact_analysis(conn, project_id, symbol, depth=2)
  │        → inject ke context sebagai "Blast Radius" section
  └─ NO  → skip (no extra context)
```

**Context format di prompt:**
```
## Blast Radius (Code Intelligence)
### `authenticate_user` (modified in src/auth/handler.rs)
  L1: login_handler (src/api/routes.rs:42)
  L1: register_handler (src/api/routes.rs:89)
  L2: main_router (src/main.rs:15) → login_handler
  L2: api_middleware (src/middleware.rs:7) → login_handler
```

**Config:** Tambah field di `ContextConfig`:
```yaml
context_chain:
  use_brain: true        # NEW: enable/disable brain enrichment (default: true)
  impact_depth: 2        # NEW: impact analysis depth (default: 2)
```

### 1.3: Affected Tests Suggestion

**File:** `src/engine/review.rs` — inject setelah blast radius

**Behavior:** Kalau index available, untuk setiap changed file:
1. Query `graph::find_callers()` untuk semua defined symbols di file
2. Filter callers yang ada di test files (naming: `*test*`, `*spec*`)
3. Inject sebagai context section:

```
## Affected Tests
- tests/auth_test.rs (callers: test_authenticate_success, test_authenticate_invalid_password)
- tests/integration/auth_spec.rs (callers: spec_login_flow)
```

**Ini memberi LLM konteks:** "File test ini mungkin perlu di-update" → LLM bisa flag missing test coverage.

### 1.4: Brain Search Enrichment (Optional, Config-Gated)

**File:** `src/engine/review.rs` — inject setelah affected tests

**Behavior:** Kalau index available + `use_brain: true`:
1. Extract function/type names dari changed symbols
2. Untuk setiap symbol, `brain_search(conn, project_id, symbol_name, 3)`
3. Filter hasil yang relevan (file berbeda dari changed files)
4. Inject top-3 related symbols sebagai context:

```
## Related Patterns (Semantic Search)
- session_manager (src/auth/session.rs:1) — manages user sessions, related to auth flow
- token_refresh (src/auth/jwt.rs:56) — JWT token refresh, depends on authenticate result
```

**Catatan:** Brain search butuh vector embeddings (`embed_project`). Di CI, embedding build dari scratch ~2-5s untuk medium project. Bisa di-skip kalau embeddings belum ada — fallback ke FTS5-only.

## Architecture

```
cora review (dengan index available)
│
├─ Parse diff → diff_chunks
│
├─ Extract symbols → outbound (what changed code calls)
│                    → inbound (what changed code defines)
│
├─ Context Chain Builder
│   ├─ Phase 1: resolve symbols (imports/types) → regex (existing)
│   ├─ Phase 2: resolve callers
│   │   ├─ 🆕 TRY index: graph::find_callers() → ContextEntry
│   │   └─ FALLBACK: regex scan (existing)
│   └─ Phase 3: assemble under token budget
│
├─ 🆕 Brain Enrichment (if index available && use_brain: true)
│   ├─ impact_analysis() → "Blast Radius" section
│   ├─ find_affected_tests() → "Affected Tests" section
│   └─ brain_search() → "Related Patterns" section
│
├─ Deterministic rules (existing, unchanged)
│
└─ LLM review prompt
    ├─ system prompt
    ├─ user prompt (diff)
    ├─ 🆕 brain context (blast radius + tests + related patterns)
    ├─ context chain text (cross-file deps)
    ├─ language-specific context
    └─ profile instructions
```

## Files to Change

| File | Change | Lines Est. |
|------|--------|------------|
| `src/engine/context/types.rs` | Add `use_brain: bool`, `impact_depth: u32` to `ContextConfig` | +15 |
| `src/engine/context/resolver.rs` | Wire `graph::find_callers()` as primary in `resolve_callers()`, fallback to regex | +40 |
| `src/engine/review.rs` | Add brain enrichment phase: impact, tests, related patterns injection | +80 |
| `src/hook/template.rs` | Add `cora index --quiet` before `cora review` in hook template | +2 |
| `src/config/schema.rs` | No change (ContextConfig already in Config struct) | 0 |

**Total estimated:** ~137 LOC new code

## Backward Compatibility

| Scenario | Behavior |
|----------|----------|
| No index (fresh CI, no `cora index`) | Identical to current — regex scan, no brain context |
| Index exists, `use_brain: false` | Identical to current — regex scan, no brain context |
| Index exists, `use_brain: true` (default) | 🆕 Index-based callers + impact + tests + brain search |
| Index exists but no embeddings | Index callers + impact + tests work; brain search falls back to FTS5-only |
| Index exists, `cora review --staged` (local) | Same as above — works for both full-diff and staged |
| CI: `cora index && cora review --ci` | Full brain enrichment ✅ |

## Pre-Commit Hook Change

**Target:** Local dev, index persistent, full brain intelligence.

### Current Hook Template (`src/hook/template.rs`)

```bash
cora review --staged --format compact
```

### Proposed Hook Template

```bash
cora index --quiet                    # NEW: incremental, ~0.014s (persistent local index)
cora review --staged --format compact  # review with brain enrichment
```

**Why this works for pre-commit:**
- Index persistent di local (`~/.codecora/`) — build sekali, incremental update seterusnya
- First run: ~1.5s full index build (one-time cost)
- Subsequent runs: ~0.014s incremental (negligible)
- Full brain intelligence available: call graph, impact analysis, brain search, affected tests

### Flow Diagram

```
Developer: git commit
    │
    ▼
Pre-commit hook fires
    │
    ├─ cora index --quiet
    │   ├─ mtime:size fingerprint check → skip unchanged files
    │   ├─ Re-index changed files (staged → source files changed)
    │   └─ Update call_graph + FTS5 + vectors
    │   ⏱ ~0.014s (incremental) or ~1.5s (first time)
    │
    ├─ cora review --staged --format compact
    │   ├─ git diff --cached → staged diff
    │   ├─ Context chain (regex + 🆕 index callers)
    │   ├─ 🆕 Brain enrichment (impact, tests, related)
    │   ├─ Deterministic rules
    │   └─ LLM review
    │   ⏱ ~5-15s (LLM API call)
    │
    └─ Exit code: 0=pass, 1=warn, 2=block
```

### Hook Template Change

**File:** `src/hook/template.rs`

```diff
- if "$CORA_BIN" review --staged --format compact 2>/dev/null; then
+ "$CORA_BIN" index --quiet 2>/dev/null || true
+ if "$CORA_BIN" review --staged --format compact 2>/dev/null; then
```

Notes:
- `cora index` uses `--quiet` (suppress output) — developer only sees review result
- `|| true` on index — if index fails (no tree-sitter lang support, etc.), review still runs
- Index is **non-blocking** — hook only blocks on review findings (exit code 2)

## CI Workflow Change (Future — NOT in scope)

CI integration (cora-review-action) akan ditambahkan di Tier 2/3 setelah
pre-commit terbukti stable. Strategy: `cora index && cora review --ci` + optional cache.

## Testing Plan

1. **Unit test:** `resolver.rs` — mock index connection, verify `find_callers()` path returns correct `ContextEntry`
2. **Unit test:** `review.rs` — verify brain context injection format
3. **Integration test:** `cora index && cora review` on cora-code repo — verify callers come from graph not regex
4. **CI test:** Run in cora-code CI pipeline — verify `cora index` step completes in < 5s
5. **Regression:** Run review without index — verify identical output to pre-change

## Open Questions

1. **Vector embedding di CI:** `embed_project()` butuh CPU static token. Untuk Rust project 156 files, estimasi ~2-5s. Apakah worth it untuk semantic search di CI, atau cukup FTS5-only?
2. **Token budget sharing:** Brain context dan context chain share `max_context_tokens`. Apakah perlu budget terpisah untuk brain enrichment?
3. **Config naming:** `use_brain` vs `intelligent_review` vs `index_enriched`?

## Non-Goals (Tier 2 & 3 — Future Work)

- **Tier 2:** Intelligent rule enhancement (unused import via index, dead code flag, breaking change detection)
- **Tier 3:** Agentic review loop (multi-step LLM calls with tool verification)
