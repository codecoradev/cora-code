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
| Symbol table (regex) | Regex extractors (15 languages) | SQLite FTS5 |
| Symbol table (AST) | Tree-sitter grammars (12 languages, opt-in) | SQLite FTS5 |
| Vector embeddings | Static token hashing (256d) | usearch HNSW index |
| Call graph | Regex scope tracking + tree-sitter AST edges | SQLite `edges` table |

```bash
cora index              # Index current project (incremental)
cora index --rebuild    # Drop and re-index from scratch
cora index --stats     # Show index statistics
cora index --watch     # Watch for file changes and auto-update
cora index --prune     # Remove symbols from deleted files
```

### Supported Languages

Cora extracts symbols using two strategies — **regex** (always available) and **tree-sitter AST** (opt-in via `--features tree-sitter` at build time):

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

**Regex-only** languages work out of the box. For **AST-accurate** extraction (full call graph, precise imports/exports), build with tree-sitter:

```bash
cargo install --git https://github.com/codecoradev/cora-code --features tree-sitter
```

### Multi-Project Index

All projects share a **single global database**:

```
~/.codecora/cora-code/
├── graph.db              # SQLite — symbols, FTS5 index, call edges
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

Phase 3 uses **static token embeddings** — a zero-dependency bag-of-tokens hashing method that produces 256-dimensional vectors. No model download, no GPU, no external service.

| Property | Value |
----------|-------|
| Dimensions | 256 (fixed) |
| Method | Bag-of-tokens hashing |
| Dependencies | None (pure Rust) |
| Quality | Good for near-duplicate detection and semantic grouping |

Future phases will add optional higher-quality embedding models (see Roadmap).

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
cora trace "handle_request" --incoming  # Trace inward (callers)
cora trace "process" --depth 4         # Limit traversal depth
cora trace --json                     # JSON output
```

Requires schema v3 edges table. When built with `--features tree-sitter`, Cora uses AST-based edge extraction for more accurate call graphs. Regex-only builds still generate edges via scope tracking.

```bash
# Build with tree-sitter for best call graph accuracy
cargo install --git https://github.com/codecoradev/cora-code --features tree-sitter
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
├── graph.db              # SQLite database
│   ├── projects         # One row per indexed project
│   ├── symbols          # All symbols from all projects
│   ├── symbols_fts      # FTS5 virtual table for keyword search
│   ├── edges            # Call relationships (caller_id → callee_id)
│   └── reviews          # Review history for tech debt tracking
└── cora_index.usearch   # usearch HNSW vector index
    ├── cora_index.usearch.keys    # Key-to-symbol-id mapping
    └── cora_index.usearch.lock    # File lock (fs2)
```

To reset everything:

```bash
rm -rf ~/.codecora/cora-code/*
cora index --rebuild
```

## Schema Versioning

The database uses automatic migrations. Current schema version: **v6**.

| Version | Changes |
|---------|---------|
| v1 | Initial symbols table + FTS5 |
| v2 | Added language column |
| v3 | Added `edges` table for call graph |
| v4 | Added `embedding_tier`, `embedding_dims`, `embedding_model`, `last_embedded_at` to projects |
| v5 | Added `reviews`, `findings`, `finding_events` tables for review history and findings tracking |
| v6 | Added index config hash column for fingerprint invalidation on config changes |