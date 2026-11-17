<div align="center">

<img src="assets/logo.png" alt="CodeCora" width="120" />

**AI-Powered Code Review CLI — BYOK**

[![GitHub stars](https://img.shields.io/github/stars/codecoradev/cora-code?style=social)](https://github.com/codecoradev/cora-code/stargazers)
[![CI](https://github.com/codecoradev/cora-code/actions/workflows/ci.yml/badge.svg)](https://github.com/codecoradev/cora-code/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cora-code.svg)](https://crates.io/crates/cora-code)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.89+-orange.svg)](https://www.rust-lang.org/)

</div>

---

**Cora** is a fast, native CLI for AI-powered code review and code intelligence — in your terminal, CI/CD, git hooks, or directly inside AI coding agents. Bring your own key, pick any model, index your codebase, and search semantically. All local, zero cloud.

## Why Cora?

- 🤖 **Multi-LLM** — OpenAI, Anthropic, Groq, Ollama, Z.AI, or any OpenAI-compatible API
- ⚡ **Native Rust** — fast binary, no runtime dependencies, cross-platform
- 🪝 **Pre-commit hooks** — catch issues before they reach CI
- 📋 **SARIF output** — upload to GitHub Code Scanning
- 🛡️ **Deterministic scanners** — built-in rules, security patterns and secret detection that run without an LLM
- 🧠 **Language-specific analysis** — tailored review guidance for Dart/Flutter, Svelte, TypeScript, Go, Rust, Python
- 🚧 **Quality gate** — configurable pass/fail thresholds for CI enforcement
- 📐 **Quality profiles** — strict, balanced, or lax presets for different project needs
- 📏 **Custom rule engine** — write your own regex rules in `.cora.yaml`
- ✂️ **Auto-chunking** — splits large PRs into reviewable chunks automatically
- 🔍 **Code Intelligence** — index symbols across 18 languages, call graph, trace, impact analysis
- 🧠 **Brain Mode** — hybrid semantic search (FTS5 + a local code embedding model + graph) with RRF fusion; the 33.5 MB model is inside the release binaries (a `cargo install` build downloads it once on first use), no API key, no GPU
- 🗄️ **Multi-project database** — one global symbol index, search across all your repos at once (vectors are one file per project)
- 🌳 **Tree-sitter** — AST-based symbol extraction for 13 languages: Rust, Go, Python, TypeScript/TSX, Java, C, C++, C#, Ruby, PHP, Scala, JavaScript, **Svelte** (via TypeScript delegation, zero extra dependency)
- 🤖 **Agent hooks** — `cora install` makes Claude Code, Hermes, pi, omp and opencode put matching code in front of the model on every prompt (hook + MCP + skill), with `--dry-run` and `--remove`
- 🔌 **MCP server** — 22 tools for AI coding agents (review, search, brain, debt, trace, dead code, graph query, agent-as-reviewer, ...)
- 💾 **Diff-hash caching** — skip repeat reviews automatically
- 🔧 **Configurable** — per-project `.cora.yaml`, global `~/.codecora/cora-code/config.yaml`, or env vars

## Quick Start

### Install

Pick **one** install method — mixing channels can leave stale binaries on your `PATH`.

| Method | When to use |
|---|---|
| **`curl … install.sh`** (recommended) | Quick standalone install; fetches the latest GitHub release binary |
| **`cargo install cora-code`** | You already have a Rust toolchain; builds from source (no bundled model) |
| **Pre-built binaries** | Manual download from [Releases](https://github.com/codecoradev/cora-code/releases) |

```bash
# Install with the quick installer
curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | sh

# Or build from source with cargo
cargo install cora-code
```

> Pin a version: `curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | CORA_VERSION=v0.21.1 sh`. The installer verifies the SHA-256 checksum (and the Sigstore signature when `cosign` is installed).
>
> **AI agents and CI:** install the release binary, do not build it from source: only the release binary contains the model and works offline.

**The code model.** Release binaries (the installer, `cora upgrade`, the archives on the Releases page) already contain the 33.5 MB `potion-code-16M-v2` model, so semantic search works right after the download, offline included; the first use unpacks it into the data directory. A `cargo install` build does not carry it (the crates.io size limit is 10 MB) and downloads it once on first use.

**Upgrading:** run `cora upgrade` (downloads the latest release, verifies its SHA-256 checksum and, when `cosign` is installed, its Sigstore signature, replaces the binary) or `cora upgrade --check` to just see if one is available. If you installed via `cargo install --path .`, re-run that instead.

**Verify which `cora` you're running** — `which -a cora` will reveal stale copies from other channels:

```bash
which -a cora            # list every `cora` on your PATH (one entry = healthy)
cora --version           # should match the latest release
```

If `which -a cora` shows more than one path (e.g. `~/.local/bin/cora` and `~/.cargo/bin/cora`), remove the one you don't want or reorder your `PATH`. See [Issue #314](https://github.com/codecoradev/cora-code/issues/314) for background.

<details>
<summary><b>macOS note — binary killed on launch (<code>Killed: 9</code>)?</b></summary>

The prebuilt `aarch64-apple-darwin` binary is not Apple-notarized. On macOS, downloaded
binaries may be tagged with `com.apple.quarantine` / `com.apple.provenance` and killed by
Gatekeeper with **no error message**.

The `install.sh` installer strips these attributes automatically. If you downloaded the
binary manually (e.g. `gh release download`), strip them yourself:

```bash
xattr -dr com.apple.quarantine /path/to/cora
xattr -dr com.apple.provenance /path/to/cora
```

A binary built on your own machine with `cargo install cora-code` is not quarantined.

</details>

### Authenticate

```bash
cora auth login
```

Pick a provider, enter your API key. Done. Provider env vars (`ZAI_API_KEY`, `OPENAI_API_KEY`, etc.) are auto-detected.

> **No key? Still useful.** `cora review` and `cora scan` run the deterministic checks (rules, secrets, security) with no LLM configured, and your AI coding agent can do the AI review itself: see [Review with your agent](docs/usage.md#review-with-your-agent-no-llm-setup).

### Review

```bash
cora review              # staged changes
cora review --base main  # vs a branch
cora review --unpushed   # unpushed commits
cora commit              # review + generate commit msg + commit
cora commit --yolo       # auto-commit, no prompts
```

### Project Config

```bash
cora init  # creates .cora.yaml + installs pre-commit hook
```

## Configuration

**Priority:** CLI flags → env vars → `.cora.yaml` (project) → `~/.codecora/cora-code/config.yaml` (global) → defaults

```yaml
# .cora.yaml
provider: zai
model: glm-5.1
focus: [security, bugs]

quality_gate:        # enforce code quality in CI
  enabled: true
  thresholds:
    max_critical: 0  # any critical finding fails the gate
```

`cora config show` prints the merged result. API keys live in `~/.codecora/cora-code/auth.toml` (chmod 600), never in `.cora.yaml`. Everything else (quality profiles, custom rules, ignore files, brain and agent settings, environment variables): **[Configuration →](https://codecora.dev/cora/docs/configuration)**

## CI/CD

[![GitHub Marketplace](https://img.shields.io/badge/Marketplace-Cora%20AI%20Code%20Review-blue?logo=github)](https://github.com/marketplace/actions/cora-ai-code-review)

```yaml
# .github/workflows/cora-review.yml
on: pull_request
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: codecoradev/cora-review-action@v1
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
          cora-api-key: ${{ secrets.CORA_API_KEY }}
```

Required secrets: `CORA_API_KEY`, `CORA_BASE_URL` (optional), `CORA_MODEL` (optional)

See [GitHub Marketplace](https://github.com/marketplace/actions/cora-ai-code-review) for full documentation.

Works on **all CI platforms** — [Gitea, GitLab, Bitbucket →](https://codecora.dev/cora/docs/examples#_07-gitea-forgejo-ci)

## Commands

| Command | Description |
|---------|-------------|
| `cora review` | Review code changes (staged, branch, commit, file) |
| `cora scan` | Scan files for issues |
| `cora commit` | Review + generate commit message + commit |
| `cora index` | Index project symbols, vectors and call graph |
| `cora brain` | Hybrid semantic search (FTS5 + vectors + graph) |
| `cora callers` / `impact` / `trace` | Who calls a symbol, what a change breaks, call chains |
| `cora install` | Configure AI coding agents (hooks, MCP, skill) |
| `cora mcp` | Start the MCP server (22 tools) for AI coding agents |
| `cora upgrade` | Self-upgrade from GitHub Releases (checksum-verified) |

Also: `explore`, `arch`, `affected`, `dead-code`, `query`, `routes`, `debt`, `findings`, `init`, `auth`, `config`, `providers`, `profile`, `serve`, `watch`, `hook`, `completion`, `upload-sarif`. See **[CLI Reference →](https://codecora.dev/cora/docs/cli-reference)** for every command and flag.

## Performance

Measured on the cora-code repository (1,996 symbols, 179 files, release build, with the real `potion-code-16M-v2` model).

| Operation | Time | Notes |
|-----------|------|-------|
| Cold index (full rebuild) | **~0.6 s** | Walk + parse + embed every symbol + write the project's vector file |
| Incremental (no changes) | **~0.13 s** | mtime:size fingerprint — skips unchanged files |
| `cora brain` (one CLI run) | **~0.2 s** | Loads the 32 MB model, then FTS5 + vector scan + graph BFS → RRF fusion. A long-running `cora mcp` keeps the model loaded |
| Binary size | **~36 MB** (**~69 MB** release binary with the model, ~38.5 MB as `tar.gz`) | Single binary; release binaries carry the 33.5 MB embedding model, a `cargo install` build downloads it once on first use |

> Measurements are indicative, not contractual. Your numbers will vary with codebase size, CPU cores, and disk I/O.

## Environment Variables

`CORA_API_KEY`, `CORA_PROVIDER`, `CORA_MODEL` and `CORA_BASE_URL` override the config (handy in CI). Provider keys such as `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GROQ_API_KEY` and `ZAI_API_KEY` are detected automatically. `CORA_EMBED_OFFLINE=1` never downloads the code model (release binaries already contain it); `CODECORA_HOME` moves the data directory. Full list: [Configuration](https://codecora.dev/cora/docs/configuration).

## Documentation

| Page | Description |
|------|-------------|
| [Getting Started](https://codecora.dev/cora/docs/getting-started) | Install, auth, first review |
| [Installation](https://codecora.dev/cora/docs/installation) | Installer, archives, upgrade, verification |
| [Usage](https://codecora.dev/cora/docs/usage) | Reviewing, scanning, review with your agent |
| [Code Intelligence](https://codecora.dev/cora/docs/code-intelligence) | Index, brain search, trace, impact |
| [AI Agent Integration](https://codecora.dev/cora/docs/agent-integration) | Hooks, MCP and skills for Claude Code, Hermes, pi, omp, opencode |
| [Configuration](https://codecora.dev/cora/docs/configuration) | Config files, env vars, priority |
| [CLI Reference](https://codecora.dev/cora/docs/cli-reference) | All commands and flags |
| [Providers](https://codecora.dev/cora/docs/providers) | Supported LLM providers |
| [Examples](https://codecora.dev/cora/docs/examples) | Common workflows & CI setup |
| [Changelog](https://codecora.dev/cora/docs/changelog) | Release history |
| [Roadmap](https://codecora.dev/cora/docs/roadmap) | Planned features |

## Star History

<a href="https://www.star-history.com/?repos=codecoradev%2Fcora-code%2Ccodecoradev%2Futeke&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=codecoradev/cora-code%2Ccodecoradev/uteke&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=codecoradev/cora-code%2Ccodecoradev/uteke&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=codecoradev/cora-code%2Ccodecoradev/uteke&type=date&legend=top-left" />
 </picture>
</a>

## Contributing

See **[CONTRIBUTING.md](CONTRIBUTING.md)** for guidelines. PRs welcome!

## License

[Apache-2.0](LICENSE)
