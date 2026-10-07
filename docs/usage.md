---
title: Usage
---

# Usage

## Review Modes

cora supports multiple review modes, each suited to a different workflow:

| Mode | Flag | Scope | Best For |
|------|------|-------|----------|
| Default | *(no flag)* | Tries staged first, then unpushed | Quick feedback |
| Staged | `--staged` | Files in git staging area | Pre-commit review |
| Unstaged | `--unstaged` | Unstaged working changes | Review work in progress |
| Unpushed | `--unpushed` | Commits not yet pushed | Review before push |
| Base Branch | `--base <branch>` | Diff against base branch | PR review workflow |
| Commit | `--commit <ref>` | Specific commit or range | Review specific changes |
| Diff File | `--diff-file <path>` | External diff file | Review patch files |

```bash
# Review staged changes (default)
$ cora review

# Review against a branch
$ cora review --base main

# Review a specific commit
$ cora review --commit HEAD

# Review from a diff file
$ cora review --diff-file pr.diff

# Full project scan (use cora scan)
$ cora scan .
```

## Output Formats

cora can output results in three formats:

- `--format pretty` — Human-readable terminal output (default)
- `--format json` — Machine-readable JSON for CI/CD pipelines
- `--format sarif` — SARIF format for GitHub Code Scanning

```bash
# JSON output example
$ cora review --staged --format json

{
  "files": [
    {
      "path": "src/auth/login.ts",
      "issues": [
        {
          "line": 42,
          "severity": "warning",
          "message": "Potential SQL injection"
        }
      ]
    }
  ],
  "summary": {
    "total_files": 3,
    "total_issues": 3
  }
}
```

## Commit Workflow

`cora commit` combines review, commit message generation, and commit into a single
command. It runs in two modes:

### HITL mode (default)

Reviews staged changes, generates a conventional commit message, then prompts:

```bash
$ git add -A
$ cora commit
🔍 Reviewing staged changes (3 files, 247 lines)...
✅ No issues found (quality score: 9.2/10)

📝 Generated commit message:
  feat(auth): add session expiry validation

Accept commit message? [Y]es / [E]dit / [N]o › y
✅ Committed: feat(auth): add session expiry validation
```

- `Y` (or Enter) — accept the message and commit
- `E` — open `$EDITOR` to edit, then commit
- `N` — abort

### YOLO mode (`--yolo`)

Auto-commits with no prompts. Use for trusted workflows (e.g. CI, rapid iteration):

```bash
$ cora commit --yolo
```

### Flags

| Flag | Description |
|------|-------------|
| `--yolo` | Auto-commit without prompts |
| `--force` | Commit even if quality gate fails |
| `--no-review` | Skip review, only generate commit message |
| `--edit` | Always open `$EDITOR` |
| `--stream` | Stream LLM response in real-time |
| `-q, --quiet` | Suppress all output except the result |

### Quality gate integration

If the quality gate is enabled and returns `FAIL`, `cora commit` blocks the
commit unless `--force` is passed:
```bash
$ cora commit --force
```

## Configuration File

The `.cora.yaml` file provides persistent configuration. Place it in your project root.

**File roles:**

| File | Purpose |
|------|---------|
| `~/.cora/auth.toml` | API key (secret, chmod 600) |
| `~/.cora/config.yaml` | Global defaults (provider, model, etc.) |
| `.cora.yaml` | Per-project overrides |

```yaml
# .cora.yaml — example
provider: zai
model: glm-5.1

focus:
  - security
  - performance

ignore:
  files:
    - "vendor/**"
    - "*.min.js"
```

## Environment Variables

Environment variables override configuration file settings:

| Variable | Description | Required |
|----------|-------------|----------|
| `CORA_API_KEY` | API key for CI (overrides auth.toml) | CI only |
| `CORA_PROVIDER` | Override the LLM provider | No |
| `CORA_MODEL` | Override the model name | No |
| `CORA_BASE_URL` | Override the API base URL | No |
| `CORA_CONFIG` | Path to alternative config file | No |
| `CORA_TRUST_PROJECT_CONFIG` | Set to `1` to honour `provider.base_url` from a discovered project `.cora.yaml` (otherwise ignored with a warning, since it would receive your API key) | No |

> **Security:** `provider.base_url` must be `https://` (plain `http://` only for `localhost`, `127.0.0.1`, `[::1]`). A `base_url` found only in a discovered project `.cora.yaml` is ignored unless `CORA_TRUST_PROJECT_CONFIG=1`; set it via `--base-url`, `CORA_BASE_URL`, an explicit `--config` file, or your global config instead.

Provider-specific keys are auto-detected: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GROQ_API_KEY`, `ZAI_API_KEY`

## Working with Monorepos

cora works well in monorepo setups. Use include patterns to limit review scope to specific packages:

```bash
# Review only the backend package
$ cora review --staged --include "packages/backend/**"

# Exclude test and generated files
$ cora review --staged --exclude "**/*.test.ts" --exclude "**/generated/**"
```

Alternatively, set include/exclude patterns in `.cora.yaml` for persistent configuration.

## Exit Codes

cora uses standard exit codes for scripting and CI integration:

| Code | Meaning | CI Behavior |
|------|---------|-------------|
| `0` | No issues found | Pass |
| `1` | Issues found | Fail (warning/error) |
| `2` | Quality gate failed or review blocked | Fail (threshold exceeded) |
| `3` | Authentication error | Fail (missing API key) |

## MCP Server

cora includes a built-in MCP server for AI coding agent integration.

```bash
# Start MCP server (JSON-RPC 2.0 over stdio)
cora mcp
```

See [Configuration → MCP Server](./configuration#mcp-server) for setup guides for Claude Code, Cursor, and other agents.

## Tech Debt Tracking

cora automatically tracks review findings over time, letting you answer "is our codebase getting better or worse?"

```bash
# Show debt report
$ cora debt

# Show trend graph over review history
$ cora debt --trend

# Machine-readable output for CI dashboards
$ cora debt --json

# Shields.io badge JSON (embed in README)
$ cora debt --badge

# Estimated fix time
$ cora debt --estimate

# Filter by date or git tag
$ cora debt --since 2026-06-01
$ cora debt --since v0.4.5

# Filter by branch
$ cora debt --branch develop
```

Snapshots are saved to `.cora/history/` after each review. Configure in `.cora.yaml`:

```yaml
debt:
  enabled: true           # default
  history_dir: .cora/history
  retention_days: 90
```

Quality score ranges from 0 (worst) to 10 (best). Each finding reduces the score:
- Critical: -2.0
- Major: -1.0
- Minor: -0.3
- Info: -0.1

Trend indicators: ▼ improving (fewer findings), ► stable, ▲ worsening (more findings).

## Tips

- Use `cora review` with no flags for the fastest pre-commit feedback
- Combine `--format json` with `--base main` in CI pipelines
- Use `cora scan . --incremental` for large codebases — only changed files are analyzed
- For large scans that hit provider token limits or rate-limits, lower the batch size with `--batch-files 10` (default: 20). Failed batches are skipped by default — pass `--no-continue-on-batch-error` to abort instead
- Use `--quiet` for minimal output and `--severity` to filter by severity level
- Use `cora auth login` to store API keys securely instead of environment variables
