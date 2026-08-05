---
layout: home

hero:
  name: Cora
  text: AI Code Review CLI
  tagline: BYOK, zero config. Multi-LLM code review, semantic search, and code intelligence — runs in your terminal, CI/CD, or AI coding agents. All local, zero cloud.
  image:
    src: /logo.png
    alt: Cora
  actions:
    - theme: brand
      text: Get Started
      link: /getting-started
    - theme: alt
      text: Installation
      link: /installation
    - theme: alt
      text: GitHub
      link: https://github.com/codecoradev/cora-code

features:
  - icon: 🤖
    title: Multi-LLM
    details: OpenAI, Anthropic, Groq, Ollama, Z.AI, or any OpenAI-compatible API. Bring your own key, pick any model.
  - icon: ⚡
    title: Native Rust
    details: Fast binary, no runtime dependencies, cross-platform. ~7.4 MB release binary.
  - icon: 🪝
    title: Pre-commit Hooks
    details: Catch issues before they reach CI. Review staged changes, unpushed commits, or any diff.
  - icon: 📋
    title: SARIF Output
    details: Upload findings to GitHub Code Scanning. Native SARIF support for CI integration.
  - icon: 🛡️
    title: Deterministic Scanners
    details: 12 built-in rules + 13 security patterns + 15 secret detection patterns — run without LLM, zero cost.
  - icon: 🧠
    title: Code Intelligence
    details: Index symbols across 15 languages. Call graph, trace, impact analysis. FTS5 + vector KNN + graph hybrid search.
  - icon: 🌳
    title: Tree-sitter
    details: "AST-based symbol extraction for 13 languages: Rust, Go, Python, TypeScript/TSX, Java, C, C++, C#, Ruby, PHP, Scala, JavaScript, Svelte."
  - icon: 🔌
    title: MCP Server
    details: 18 tools for AI coding agents — review, search, brain, debt, trace, dead code, graph query. Works with Claude, Cursor, Windsurf, and other MCP clients.
  - icon: 📐
    title: Quality Profiles
    details: Strict, balanced, or lax presets. Configurable quality gate with pass/fail thresholds for CI enforcement.
  - icon: 💾
    title: Diff-hash Caching
    details: Skip repeat reviews automatically. Incremental index in ~6ms with mtime:size fingerprint.
  - icon: 🚧
    title: Quality Gate
    details: Configurable pass/fail thresholds for CI enforcement. Max critical, max security findings, per-category actions.
  - icon: 🔧
    title: Configurable
    details: Per-project .cora.yaml, global ~/.cora/config.yaml, or env vars. Custom regex rules in config.
---
