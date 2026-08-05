---
title: Getting Started
---

# Getting Started

## Quick Start

Get up and running with cora in three simple steps.

1. **Install cora** — Single binary, no runtime dependencies:
   `curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | sh`

2. **Authenticate** — Run `cora auth login` to pick your provider and enter your API key.
   Cora stores the API key in `~/.cora/auth.toml` (never committed to git) and provider settings in `~/.cora/config.yaml`.

3. **Review** — Analyze your staged changes:
   `cora review`

## Authentication — `cora auth login`

The interactive login guides you through provider selection:

```bash
$ cora auth login

🔑 Cora Auth Setup
   Choose your LLM provider:

  [1] openai
  [2] anthropic
  [3] groq
  [4] ollama
  [5] zai
  [6] custom

  Select provider [1-6]: 5

  → Provider: zai
  🔑 Found ZAI_API_KEY in environment
     Use it? [Y/n]: Y
     ✅ Using ZAI_API_KEY from environment

  Model [glm-5.1]:          ← press Enter to accept default
  Base URL [https://api.z.ai/api/coding/paas/v4]:  ← press Enter to accept default

✅ API key saved to ~/.cora/auth.toml
   Provider: zai | Model: glm-5.1 | Base: https://api.z.ai/api/coding/paas/v4
```

| Term | Description |
|------|-------------|
| Known providers | Provider env vars auto-detected (e.g. `ZAI_API_KEY`) |
| Custom provider | Enter your own base URL, model name, and API key |
| API key | Saved to `~/.cora/auth.toml` (secret) |
| Provider settings | Saved to `~/.cora/config.yaml` |

Check your auth status anytime:

```bash
$ cora auth status
✅ API key is configured.
   Source: ~/.cora/auth.toml

   Provider: openai
   Model: gpt-4o-mini
   Base URL: https://api.openai.com/v1
```

You can also use provider environment variables: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `ZAI_API_KEY`, etc. `CORA_API_KEY` is reserved for CI use.

## Understanding the Output

cora outputs a structured, color-coded summary of findings for each file reviewed.

```bash
$ cora review --staged

Analyzing 3 files...
✓ src/auth/login.ts — 2 issues found
  ⚠ Line 42: Potential SQL injection
  ⚠ Line 87: Hardcoded secret
✓ src/utils/parser.ts — clean
✓ src/api/routes.ts — 1 issue found
  ✗ Line 23: Missing error handling

3 issues found in 3 files
```

Each line in the output contains:

| Term | Description |
|------|-------------|
| file path | The relative path to the file being reviewed |
| line number | Specific line where the issue was found |
| severity | Suggestion (⚠), Warning (⚠), or Error (✗) |
| description | Brief explanation of the issue |

## Project Configuration — `cora init`

Create a `.cora.yaml` config file in your project root. **Automatically installs a pre-commit hook** that runs `cora review --staged --format compact` before each commit. Use `--no-hook` to skip hook installation.

```bash
$ cora init
✅ Created .cora.yaml with example configuration.
✅ Pre-commit hook installed at .git/hooks/pre-commit
```

Set provider, model, and base URL directly in `.cora.yaml` (no nested section needed):

```yaml
# .cora.yaml — shortcut format
provider: openai
model: gpt-4o-mini
base_url: https://api.openai.com/v1
```

Key configuration options:

| Key | Description |
|-----|-------------|
| `focus` | Review focus areas: security, performance, bugs, best_practice |
| `ignore` | File patterns and rules to skip |
| `hook` | Pre-commit hook settings: mode, severity threshold, max diff size |
| `llm` | LLM parameters: temperature, max_tokens, timeout |
| `brain.embedding` | Embedding backend: `auto`, `hashing` (256d), or `pretrained` (768d) |

## Next Steps

Now that you have cora running, explore these topics to get the most out of it:

- [Installation](./installation) — install options and shell completions
- [Usage](./usage) — review modes, output formats, and configuration
- [Configuration](./configuration) — full .cora.yaml reference, quality gate, security scanner, MCP server
- [Code Intelligence](./code-intelligence) — symbol index, call graph, brain mode, embedding backends
- [Providers](./providers) — setting up OpenAI, Anthropic, Groq, Ollama, and Z.AI
- [CLI Reference](./cli-reference) — full command documentation
- [Examples](./examples) — CI/CD setup for GitHub, GitLab, Gitea, Bitbucket

### Daily Workflow

Once set up, your daily workflow becomes:

```bash
git add -A
cora commit          # review + generate commit message + commit
```

Or skip the prompt in CI/trusted workflows:

```bash
cora commit --yolo    # auto-commit, no prompts
```

## AI Agent Integration

cora includes a built-in MCP server for AI coding agents. After installation:

```bash
# Start MCP server
$ cora mcp
```

Configure in Claude Code (`.claude/settings.json`) or Cursor (`.cursor/mcp.json`):

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

See [Configuration → MCP Server](./configuration#mcp-server) for full setup guides.
