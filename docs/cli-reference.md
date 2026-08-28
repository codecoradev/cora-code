---
title: CLI Reference
---

# CLI Reference

Complete command reference for the cora CLI.

## Global Flags

| Flag | Description |
|------|-------------|
| `--config` `<path>` | Override config file path (default: `.cora.yaml`) |
| `--format` `<fmt>` | Output format: pretty, json, compact, sarif |
| `--no-color` | Disable colored output |
| `--provider` `<name>` | Override provider |
| `--model` `<name>` | Override model |
| `--base-url` `<url>` | Override API base URL |
| `--api-key` `<key>` | Override API key |
| `--verbose` | Enable debug logging |

## Commands

### Setup & Config

| Command | Description |
|---------|-------------|
| `cora init` | Create `.cora.yaml` config file and install pre-commit hook |
| `cora init --force` | Overwrite existing config file |
| `cora init --no-hook` | Skip pre-commit hook installation |
| `cora config show` | Show resolved configuration |
| `cora config show --global` | Show global config (`~/.cora/config.yaml`) |
| `cora config show --project` | Show project config (`.cora.yaml`) |
| `cora config set` `<key>` `<value>` | Set a config value |
| `cora config set` `<key>` `<value>` `--global` | Write to global config instead of project |
| `cora config validate` | Validate configuration and report status |
| `cora auth login` | Save API key interactively |
| `cora auth login --provider` `<name>` `--api-key` `<key>` | Non-interactive login |
| `cora auth login --model` `<model>` | Set model with provider |
| `cora auth login --base-url` `<url>` | Custom API endpoint |
| `cora auth login --force` | Overwrite existing key without confirmation |
| `cora auth status` | Check current auth status |
| `cora auth remove` | Remove stored API key |
| `cora providers` | List detected AI providers |
| `cora install` | Auto-detect and configure AI coding agents for Cora MCP |
| `cora install --list` | List detected agents without installing |
| `cora install --agents` `"cline,cursor"` | Install specific agents |
| `cora install --dry-run` | Show what would be changed |
| `cora install --force` | Overwrite existing cora entry |
| `cora install --yes` | Install ALL detected agents (non-interactive) |
| `cora hook install` | Install pre-commit hook |
| `cora hook uninstall` | Remove pre-commit hook |
| `cora completion` `<shell>` | Generate shell completions (bash/zsh/fish/powershell) |

### Review & Scan

| Command | Description |
|---------|-------------|
| `cora review` | Review code changes (default: tries staged, then unpushed) |
| `cora review --staged` | Review staged git changes |
| `cora review --unstaged` | Review unstaged working changes |
| `cora review --unpushed` | Review unpushed commits |
| `cora review --base` `<branch>` | Compare current branch against target |
| `cora review --commit` `<ref>` | Review specific commit or range |
| `cora review --diff-file` `<path>` | Review from a diff file |
| `cora review --upload` | Review and upload SARIF to GitHub Code Scanning |
| `cora review --no-auto-chunk` | Disable auto-chunking for large diffs |
| `cora review --progress` | Output NDJSON progress events to stderr |
| `cora review --quiet` | Suppress all output except result |
| `cora review --output-file` `<path>` | Write output to file instead of stdout |
| `cora review --severity` `<level>` | Filter by min severity (info/minor/major/critical) |
| `cora review --no-cache` | Disable review caching |
| `cora review --ci` | CI mode: skip diff size limit, exit 2 if any findings |
| `cora review --max-diff-size` `<chars>` | Override max diff size |
| `cora review --memory` | Recall project patterns from Uteke before review |
| `cora review --learn` | Save findings to Uteke after review (implies `--memory`) |
| `cora commit` | Review staged + generate commit message + commit (HITL prompt) |
| `cora commit --yolo` | Auto-commit without prompts |
| `cora commit --force` | Commit even if quality gate fails |
| `cora commit --no-review` | Skip review, only generate commit message |
| `cora commit --edit` | Always open `$EDITOR` to edit message |
| `cora commit --stream` | Stream LLM response in real-time |
| `cora commit --quiet` | Suppress all output except result |
| `cora scan` `[--path <dir>]` | Scan files for issues (default: current directory) |
| `cora scan --include` `"src/**/*.rs"` | Include glob patterns |
| `cora scan --exclude` `"vendor/**"` | Exclude glob patterns |
| `cora scan --extensions` `"ts,js"` | Additional file extensions to scan |
| `cora scan --incremental` | Scan only files changed since last scan |
| `cora scan --focus` `security` | Override focus areas |
| `cora scan --batch-files` `N` | Max files per LLM batch (default: 20) |
| `cora scan --no-continue-on-batch-error` | Abort on batch failure (default: skip and continue) |

### Code Intelligence

See [Code Intelligence](./code-intelligence) for detailed usage.

| Command | Description |
|---------|-------------|
| `cora index` | Index project symbols into SQLite + usearch |
| `cora index --rebuild` | Rebuild index from scratch |
| `cora index --watch` | Auto-sync file watcher (2s poll interval) |
| `cora index --stats` | Show index statistics (symbol count, languages, DB size) |
| `cora index --prune` | Remove stale entries for deleted files |
| `cora explore` `<query>` | Keyword search (FTS5) over symbol names |
| `cora explore --kind` `function` | Filter by symbol kind |
| `cora explore --file` `"src/"` | Filter by file path prefix |
| `cora explore --language` `rust` | Filter by language |
| `cora explore --limit` `N` | Max results (default: 50) |
| `cora brain` `<query>` | Hybrid search: FTS5 + vector + graph → RRF fusion |
| `cora brain --limit N` | Max results (default: 20) |
| `cora callers` `<symbol>` | Find all callers of a symbol (reverse call graph) |
| `cora callers --limit N` | Max callers to return (default: 50) |
| `cora impact` `<symbol>` | Analyze blast radius of changing a symbol |
| `cora impact --depth N` | Traversal depth (default: 3) |
| `cora trace` `<symbol>` | Trace call chains (depth-limited BFS) |
| `cora trace --direction incoming` | Trace callers instead of callees |
| `cora trace --depth N` | Max hops (default: 3) |
| `cora arch` | Architecture overview — modules, edge types, top connectors |
| `cora affected` `<files...>` | Find test files affected by source changes |
| `cora affected --stdin` | Read changed files from stdin (pipe from `git diff --name-only`) |
| `cora affected --filter` `"*test*"` | Custom test file glob pattern |
| `cora dead-code` | Detect dead code — functions/methods with zero callers (public API surface skipped by default; honors ignore.files via the index) |
| `cora dead-code --include-pub` | Include public API surface (pub/export items) in results |
| `cora dead-code --include-tests` | Include test functions in results |
| `cora dead-code --min-lines N` | Filter out tiny functions |
| `cora query` `"main -> *"` | Query the code graph with simple patterns |
| `cora query --limit N` | Max results (default: 50) |
| `cora routes` | List detected HTTP routes (Axum, Actix, Express, FastAPI, Flask, Go) |
| `cora routes --method GET` | Filter by HTTP method |
| `cora routes --prefix /api` | Filter by path prefix |

### Quality Profiles

| Command | Description |
|---------|-------------|
| `cora profile list` | List available quality profiles |
| `cora profile show` `<name>` | Show details of a specific profile |
| `cora profile validate` `<path>` | Validate a custom profile YAML file |

### Findings & Debt

| Command | Description |
|---------|-------------|
| `cora findings list` | Show open findings |
| `cora findings list --all` | Show all findings including resolved |
| `cora findings list --severity major` | Filter by severity |
| `cora findings list --file "src/main.rs"` | Filter by file |
| `cora findings list --json` | JSON output |
| `cora findings stats` | Summary counts with resolution rate |
| `cora findings dismiss <id>` | Mark finding as won't-fix |
| `cora findings dismiss <id> --reason "..."` | Dismiss with reason |
| `cora findings reopen <id>` | Reopen a dismissed/resolved finding |
| `cora debt` | Show tech debt report from review history |
| `cora debt --json` | Debt report as JSON (for CI/dashboards) |
| `cora debt --trend` | Quality score trend graph |
| `cora debt --badge` | Shields.io badge JSON endpoint |
| `cora debt --estimate` | Show estimated fix time |
| `cora debt --since v0.4.5` | Filter by git tag or date |
| `cora debt --branch main` | Filter by branch |
| `cora upload-sarif` `<file>` | Upload SARIF to GitHub Code Scanning |

### MCP Server

| Command | Description |
|---------|-------------|
| `cora mcp` | Start MCP server for AI coding agents (Claude Code, Cursor, Windsurf) |
| `cora serve` | Start MCP server with auto-reindex on startup |

## Quick Examples

```bash
# Review staged changes (what's about to be committed)
$ cora review --staged
```

```bash
# Compare your feature branch against main
$ cora review --base main
```

```bash
# Full project scan with incremental caching
$ cora scan --incremental
```

```bash
# Install pre-commit hook
$ cora hook install
```

```bash
# Review + auto-generate commit message + commit
$ cora commit

# YOLO mode — auto-commit, no prompts
$ cora commit --yolo
```

```bash
# Index your project and search with Brain Mode
$ cora index
$ cora brain "error handling"
$ cora brain "TokenEmbedding" --json --limit 5
```

```bash
# Trace call chains and view architecture
$ cora trace main
$ cora arch
```

```bash
# Find callers and analyze impact
$ cora callers handle_request
$ cora impact process_order --depth 5
$ cora affected src/order.rs src/payment.rs
```
