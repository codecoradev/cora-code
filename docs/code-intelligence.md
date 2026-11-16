---
title: Code Intelligence & Brain Mode
---

# Code Intelligence & Brain Mode

Cora includes a built-in symbol index, call graph, and hybrid semantic search engine. No external services required — everything runs locally.

## Quick Start

```bash
# 1. Index your project (extracts symbols, builds vector index)
cora index

# 2. Search symbols (keyword)
cora explore "authenticate"

# 3. Semantic search (hybrid: keyword + vector + graph)
cora brain "error handling pattern"

# 4. Trace call chains
cora trace main

# 5. View architecture overview
cora arch
```

## Indexing

### `cora index`

Scans your project, extracts symbol definitions (functions, structs, enums, traits, etc.), and builds:

| Component | Technology | Storage |
|-----------|-----------|---------|
| Symbol table (regex) | Regex extractors (18 languages) | SQLite FTS5 |
| Symbol table (AST) | Tree-sitter grammars (12+ languages, enabled by default) | SQLite FTS5 |
| Vector embeddings | Static hashing (256d) or pretrained nomic (768d), runtime-selectable | usearch HNSW index |
| Call graph | Regex scope tracking + tree-sitter AST edges | SQLite `edges` table |

```bash
cora index              # Index current project (incremental)
cora index --rebuild    # Drop and re-index from scratch
cora index --stats     # Show index statistics
cora index --watch     # Watch for file changes and auto-update
cora index --prune     # Remove symbols from deleted files
```

### Supported Languages

Cora extracts symbols using two strategies — **regex** (always available) and **tree-sitter AST** (enabled by default in release builds; can be disabled at build time):

| | Regex Extraction | AST (Tree-sitter) |
|--|-----------------|-------------------|
| **Rust** | ✅ | ✅ |
| **Go** | ✅ | ✅ |
| **Python** | ✅ | ✅ |
| **TypeScript/TSX** | ✅ | ✅ |
| **Svelte** | ✅ | ✅ (via TypeScript) |
| **Java** | ✅ | ✅ |
| **C** | ✅ | ✅ |
| **C++** | ✅ | ✅ |
| **C#** | ✅ | ✅ |
| **Ruby** | ✅ | ✅ |
| **PHP** | ✅ | ✅ |
| **Scala** | ✅ | ✅ |
| **JavaScript** | ✅ | ✅ |
| **Swift** | ✅ | — |
| **Kotlin** | ✅ | — |
| **Dart** | ✅ | — |
| **Lua** | ✅ | — |
| **Zig** | ✅ | — |

**Regex-only** languages work out of the box. Tree-sitter AST extraction is **enabled by default** in normal builds (`default = ["tree-sitter"]` in `Cargo.toml`); only minimal/no-default-feature builds fall back to regex-only. To explicitly enable it:

```bash
cargo install --git https://github.com/codecoradev/cora-code --features tree-sitter
```

### Multi-Project Index

All projects share a **single global database**:

```
~/.codecora/cora-code/
├── cora.db               # SQLite — symbols, FTS5 index, call edges
└── cora_index.usearch   # usearch HNSW — 256d vector embeddings
```

Index multiple repos and they all become searchable from any directory:

```bash
cd ~/repos/my-api && cora index        # Index Go project
cd ~/repos/my-app && cora index        # Index Flutter project
cd ~/repos/my-lib && cora brain "auth" # Results from ALL indexed projects
```

### Environment Override

```bash
CODECORA_HOME=/custom/path cora index  # Use custom data directory
```

## Search Commands

### `cora explore` — Keyword Search

FTS5 full-text search over symbol names and signatures.

```bash
cora explore "authenticate"         # Search by name
cora explore --kind function        # Filter by symbol kind
cora explore --language rust        # Filter by language
cora explore --limit 20              # Max results
cora explore --json                 # JSON output
```

### `cora brain` — Hybrid Semantic Search

Combines three search signals into ranked results via **Reciprocal Rank Fusion (RRF, k=60)**:

| Signal | Engine | What it finds |
--------|--------|--------------|
| FTS5 | SQLite keyword match | Exact name/signature matches |
| Vector KNN | usearch HNSW (cosine) | Semantically similar code |
| Graph BFS | Call graph traversal | Related symbols (callers/callees) |

```bash
cora brain "error handling"              # Top 20 results
cora brain "TokenEmbedding" --json       # JSON output
cora brain "parsing" --limit 10          # Custom limit
```

#### JSON Output

```json
[
  {
    "symbol_id": 935,
    "name": "normalize_error_handling",
    "kind": "function",
    "file": "src/engine/debt_tracker.rs",
    "line": 685,
    "score": 0.0164,
    "signals": ["fts"]
  }
]
```

The `signals` field shows which search signals matched: `"fts"`, `"vector"`, `"graph"`, or combinations like `["fts", "vector"]`.

#### Embedding Engine

Cora embeds every symbol (its name and signature) locally, with no API key and no GPU. The backend is selectable via `brain.embedding` in `.cora.yaml`.

| Backend | Dimensions | Method | When to use |
|---------|------------|--------|-------------|
| `potion` (default via `auto`) | 256 | `potion-code-16M-v2`, a static code model: tokens are looked up in a learned table and averaged. Downloaded once (33.5 MB) | Almost always: it finds the right function from a description clearly more often than keyword search alone |
| `hashing` | 256 | Bag-of-tokens hashing | Offline fallback, near-duplicate detection. A weak signal for search |
| `pretrained` | 768 | nomic-embed-code distilled (needs the `pretrained-embed` build feature) | Legacy |

```yaml
# .cora.yaml
brain:
  embedding: auto   # auto | potion | hashing | pretrained
```

**Why this model.** It was chosen by measurement, on 1,639 queries (hand-written questions and doc comments of this repository, of 8 other Rust crates and of the Python standard library) with the documents cora indexes (name and signature). Alone it beat keyword search by +0.07 to +0.12 in MRR@10, with intervals that exclude zero on every set, and fused with keyword search at full weight it is better by +0.04 to +0.09. The previous embeddings were neutral on this repository and harmful elsewhere (down to -0.09), which is why `hashing` and `pretrained` now count for a quarter of a vote. On `cora brain` itself, on this repository, MRR@10 rose from 0.22 to 0.32 (hand-written questions) and from 0.39 to 0.52 (doc comments).

Embedding is cheap: tens of thousands of symbols per second on a few CPU cores, about 80 MB of memory for the model, and 256-dimensional vectors (about 1 KB per symbol).

#### Where it is stored

| What | Where |
|---|---|
| Symbols, edges, call graph, review history, per-project embedding metadata (model, dimensions, time) | SQLite database `cora.db` in the data directory (`~/.codecora/cora-code/`) |
| Keyword index | An FTS5 virtual table in the same database |
| Vectors | One file per model next to the database, keyed by the symbol's database id: `cora_index.potion-code-v2.usearch` (HNSW graph over f32 vectors, via usearch) or, with `brain.vector_store: vecq`, a `.vecq` file (quantized brute-force scan, about 5x smaller). The file is shared by all projects and a lock serializes writers |
| The model | `models/potion-code-16M-v2/` in the data directory |

A search embeds the question, asks the vector file for the nearest symbols, keeps those of the current project, and fuses them with the keyword and graph results by reciprocal rank fusion. A symbol is re-embedded only when its name, signature or the model changed.

#### Where it is used

- `cora brain` and the MCP tool `cora.brain_search`: the full hybrid, for questions in words.
- `cora explore` with a multi-word query: the keyword results, completed with a separate block of matches by meaning (`--no-semantic` turns it off; `--json` marks each result `via: keyword | semantic`).
- `cora review`: related code and tests found for the changed symbols.

## Call Graph Commands

### `cora callers` — Who Calls This?

Find all symbols that call a given symbol.

```bash
cora callers "authenticate"    # Who calls authenticate?
cora callers --json            # JSON output
cora callers --limit 50        # Max results
```

### `cora impact` — What Breaks?

Analyze the blast radius of changing a symbol.

```bash
cora impact "parse_response"          # What depends on this?
cora impact "parse_response" --depth 3  # 3 levels up the call graph
cora impact --json                     # JSON output
```

### `cora trace` — Call Chain Tracing

Trace execution paths through the call graph.

```bash
cora trace "main"                    # Trace outward (callees)
cora trace "handle_request" --direction incoming  # Trace inward (callers)
cora trace "process" --depth 4         # Limit traversal depth
cora trace --json                     # JSON output
```

Requires schema v3 edges table. Cora uses tree-sitter AST-based edge extraction for more accurate call graphs (enabled by default). Regex-only builds (no-default-features) still generate edges via scope tracking.

### Call Graph Semantics & Limitations

**Cross-file and multi-level.** The call graph is stored per-edge with file and line, and `impact`/`trace` traverse it with BFS plus cycle protection. A caller three abstraction layers away from the symbol — through any number of intermediate files — is reachable by raising `--depth` (see [Impact Depth Guidance](/configuration#impact-depth-guidance)).

**Cross-project fallback.** If a symbol has no callers in the current project's scope, `cora callers` automatically falls back to a lookup across **all indexed projects** and reports which project each match came from. Useful when a symbol is defined in one repo and consumed from another (e.g. a shared crate consumed via workspace or git dependency).

**Static analysis — what it cannot see.** Edges are extracted statically, so they can be missing for:

- **Dynamic dispatch** — trait objects (`dyn Trait`), virtual/interface calls. The concrete callee is only known at runtime.
- **Callbacks and function pointers** — a function passed as an argument and invoked elsewhere has no direct edge to its eventual call-site.
- **Reflection / string-based dispatch** — unresolvable statically by definition.

In these cases the traversal chain can end early, and `cora impact` may under-report the blast radius. For dynamic-dispatch-heavy code, treat the call graph as a lower bound and confirm with tests or manual tracing.

```bash
# Build with tree-sitter for best call graph accuracy
cargo install --git https://github.com/codecoradev/cora-code   # tree-sitter included by default
cora index --rebuild  # Re-index to get AST edges
```

### `cora arch` — Architecture Overview

Display a high-level view of the codebase structure.

```bash
cora arch          # Human-readable overview
cora arch --json   # JSON output
```

Shows: module breakdown, edge types (calls, imports), and top connector symbols.

### `cora dead-code` — Dead Code Detection

Find functions and methods that have zero callers — candidates for removal.

```bash
cora dead-code                     # Find dead functions
cora dead-code --include-tests     # Include test functions (test_*, *_test)
cora dead-code --min-lines 10      # Filter out tiny functions
cora dead-code --json              # JSON output
```

> **Tip:** Use `analysis.entry_point_patterns` in `.cora.yaml` to mark entry points (e.g. `*Handler`, `main`) so they're not flagged as dead code.

### `cora query` — Code Graph Query

Query the call graph with simple pattern syntax.

```bash
cora query "main -> *"             # What does main call?
cora query "* -> authenticate"     # What calls authenticate?
cora query "MyStruct"              # Find all edges involving MyStruct
cora query --limit 100 "main -> *"
```

Pattern syntax: `source -> target`, where each side can be a symbol name or `*` (wildcard).

### `cora routes` — HTTP Route Listing

List detected HTTP routes from framework annotations. Supports Axum, Actix, Express, FastAPI, Flask, and Go (net/http, gin, echo, chi).

```bash
cora routes                        # All routes
cora routes --method GET           # Filter by HTTP method
cora routes --prefix /api          # Filter by path prefix
cora routes --json                 # JSON output
```

## Test Impact Analysis

### `cora affected`

Find tests that are impacted by changed files.

```bash
cora affected                              # From git diff
cora affected src/auth.rs src/api.rs      # Specific files
cora affected --stdin                      # Pipe from git diff --name-only
cora affected --filter "*test*"            # Custom test file pattern
cora affected --json                       # JSON output
```

## MCP Integration

All code intelligence features are available as MCP tools for AI coding agents:

| Tool | Description |
|------|-------------|
| `cora.search_symbols` | FTS5 symbol search |
| `cora.find_callers` | Find callers of a symbol |
| `cora.find_impact` | Impact analysis |
| `cora.find_affected_tests` | Test impact analysis |
| `cora.index_status` | Index statistics |
| `cora.brain_search` | Hybrid semantic search |
| `cora.get_project_info` | Project metadata & config |
| `cora.get_memory` | Review memory recall |

## Data Directory

```
~/.codecora/cora-code/
├── cora.db               # SQLite database (renamed from graph.db in v0.9)
│   ├── projects         # One row per indexed project
│   ├── symbols          # All symbols from all projects
│   ├── symbols_fts      # FTS5 virtual table for keyword search
│   ├── edges            # Call relationships (caller_id → callee_id)
│   ├── reviews          # Review history for tech debt tracking
│   └── findings         # Review findings + finding_events
└── cora_index.usearch   # usearch HNSW vector index
    └── cora_index.keys           # Key-to-symbol-id mapping (lock is held on the .usearch file itself, fs2)
```

To reset everything:

```bash
rm -rf ~/.codecora/cora-code/*
cora index --rebuild
```

## Schema Versioning

The database uses automatic migrations. Current schema version: **v7**.

| Version | Changes |
|---------|---------|
| v1 | Initial symbols table + FTS5 |
| v2 | Added language column |
| v3 | Added `edges` table for call graph |
| v4 | Added `embedding_tier`, `embedding_dims`, `embedding_model`, `last_embedded_at` to projects |
| v5 | Added `reviews`, `findings`, `finding_events` tables for review history and findings tracking |
| v6 | Added index config hash column for fingerprint invalidation on config changes; rebuilt FTS5 with `file` column for camelCase search |
| v7 | Added `embed_fingerprint TEXT` column to `symbols` for incremental per-symbol embedding |