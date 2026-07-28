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

| Command | Description |
|---------|-------------|
| `cora init` | Create `.cora.yaml` config file |
| `cora commit` | Review staged + generate commit message + commit (HITL prompt) |
| `cora commit --yolo` | Auto-commit without prompts (YOLO mode) |
| `cora commit --force` | Commit even if quality gate fails |
| `cora commit --no-review` | Skip review, only generate commit message |
| `cora commit --edit` | Always open `$EDITOR` to edit message |
| `cora review` | Review code changes (default: staged files) |
| `cora review --staged` | Review staged git changes explicitly |
| `cora review --unstaged` | Review unstaged working changes |
| `cora review --unpushed` | Review unpushed commits |
| `cora review --base` `<branch>` | Compare current branch against target |
| `cora review --commit` `<ref>` | Review specific commit or range |
| `cora review --diff-file` `<path>` | Review from a diff file |
| `cora review --upload` | Review and upload SARIF to GitHub Code Scanning |
| `cora scan` `<path>` | Scan files for issues |
| `cora scan .` `[--incremental]` | Scan only changed files |
| `cora scan .` `[--batch-files N]` | Max files per LLM batch (default: 20). Lower to work around provider token limits |
| `cora scan .` `[--no-continue-on-batch-error]` | Abort the scan when a batch fails to parse (default: skip and continue) |
| `cora config show` | Show resolved configuration |
| `cora config show --global` | Show global config (`~/.cora/config.yaml`) |
| `cora config show --project` | Show project config (`.cora.yaml`) |
| `cora config set` `<key>` `<value>` | Set a config value |
| `cora hook install` | Install pre-commit hook |
| `cora hook uninstall` | Remove pre-commit hook |
| `cora auth login` | Save API key to `~/.cora/auth.toml` |
| `cora auth status` | Check current auth status |
| `cora auth remove` | Remove stored API key |
| `cora providers` | List detected AI providers |
| `cora upload-sarif` `<file>` | Upload SARIF to GitHub Code Scanning |
| `cora debt` | Show tech debt report from review history |
| `cora debt --json` | Debt report as JSON (for CI/dashboards) |
| `cora debt --trend` | Quality score trend graph |
| `cora debt --badge` | Shields.io badge JSON endpoint |
| `cora debt --estimate` | Show estimated fix time |
| `cora debt --since v0.4.5` | Filter by git tag or date |
| `cora debt --branch main` | Filter by branch |
| `cora findings list` | Show open findings (use `--all`, `--severity`, `--file`, `--json`) |
| `cora findings stats` | Summary counts with resolution rate (`--json`) |
| `cora findings dismiss <id>` | Mark finding as won't-fix (optional `--reason`) |
| `cora findings reopen <id>` | Reopen a dismissed/resolved finding |
| `cora arch` | Architecture overview — modules, edge types, top connectors |
| `cora trace` `<symbol>` | Trace call chains from a symbol (depth-limited BFS) |
| `cora brain` `<query>` | Hybrid search: FTS5 + vector + graph → RRF fusion |
| `cora brain` `--json` | Brain search as JSON |
| `cora brain` `--limit N` | Max results (default: 20) |
| `cora index` | Index project symbols into SQLite + usearch |
| `cora index --rebuild` | Rebuild index from scratch |
| `cora index --watch` | Auto-sync file watcher (2s poll interval) |
| `cora index --stats` | Show index statistics (symbol count, languages, DB size) |
| `cora index --prune` | Remove stale entries for deleted files |
| `cora callers` `<symbol>` | Find all callers of a symbol (reverse call graph) |
| `cora callers` `--limit N` | Max callers to return (default: 50) |
| `cora impact` `<symbol>` | Analyze blast radius of changing a symbol |
| `cora impact` `--depth N` | Traversal depth (default: 3) |
| `cora affected` `<files...>` | Find test files affected by source changes |
| `cora completion` `<shell>` | Generate shell completions (bash/zsh/fish) |
| `cora mcp` | Start MCP server for AI coding agents (Claude Code, Cursor, Windsurf) |

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
