# Contributing to Cora

Cora is a solo-maintained project with a strong product direction. Contributions are welcome, but **alignment matters more than volume**.

This document helps you decide *whether* and *how* to contribute in a way that's likely to get merged, so neither of us wastes time.

## How this project is run

- Cora has one active maintainer ([@ajianaz](https://github.com/ajianaz)).
- Review bandwidth is limited.
- Not every contribution can be accepted, even if it's technically correct. Alignment with project direction matters as much as code quality.
- For scope and direction, check open issues and [ROADMAP.md](ROADMAP.md) if available. Read them before opening anything non-trivial.

This is normal for a solo project. A "no" on a PR is not personal.

## Quick start

```bash
# Prerequisites: Rust 1.85+ (via https://rustup.rs), Git
git clone https://github.com/codecoradev/cora-code.git
cd cora-code
cargo build
cargo test
```

> **Note:** Cora uses the `tree-sitter` feature for intelligent code analysis. Some tests require this feature: `cargo test --features tree-sitter`.

## Where to discuss

Use GitHub Issues for tracking concrete bugs and features. For design discussions or "should I work on X?", open an issue first.

## What makes a good contribution

These get merged fast:

- **Bug fixes** with clear reproduction steps and tests.
- **Docs / typos / small UX fixes** — open a PR directly.
- **Pre-discussed features** — alignment in an issue first.
- **Small, focused changes** — easy to review, low risk.

If your change is small and obvious (typo, narrow bugfix, small docs change), open a PR directly. No issue required.

## Keep changes focused

**Only change what's needed to accomplish your stated goal.**

If you're fixing a bug in `index/symbols.rs`, don't also:

- Reformat other files
- Clean up unrelated code
- Fix lint issues in files you didn't need to touch
- Combine multiple unrelated fixes in one PR

**One PR = one logical change.** Multi-concern PRs will be asked to split.

## Discuss first (required for larger changes)

For anything beyond a small fix, **discussion is required before opening a PR**. This includes:

- New features
- API changes or new MCP tools
- Refactors or "cleanup" work
- Performance rewrites
- Architectural changes
- Anything touching many files or subsystems
- Changes to the index engine, code graph, embedding, or schema migration subsystems

Pull requests with significant unsolicited changes will be closed without detailed review. This isn't meant to discourage contribution. It ensures alignment before significant work goes in.

A 10-minute conversation saves a 500-line PR that doesn't fit the roadmap.

## Quality bar

Every PR is reviewed against:

- `cargo fmt --all -- --check` — must be clean
- `cargo clippy --all-targets --features tree-sitter -- -D warnings` — must be clean
- `cargo test --features tree-sitter` — must pass
- `cargo build --release --features tree-sitter` — must compile
- No new heavy dependencies without justification
- No perf regressions in hot paths: indexing, FTS5 search, symbol extraction, embedding

If you're not sure how to measure perf or what counts as a hot path, ask in an issue. Better to confirm than get bounced.

## Changes to core subsystems require a test

The most common way a PR breaks Cora is a **local fix with global blast radius**: the diff solves one case, reads fine, passes clippy, and silently breaks the same subsystem in other cases. Review alone does not catch these. A test does.

If your change touches behavior in any of these load-bearing paths, the PR must add or extend a test:

- **Index engine (src/index/)**: SQLite writes, schema migrations, FTS5, symbol extraction
- **Code graph (src/index/graph.rs)**: call graph construction, dead-code detection
- **Embedding (src/embed/)**: ONNX inference, tokenizer, dimension handling
- **Schema migration (src/index/schema.rs)**: version upgrades, data migration
- **Config (src/config/)**: YAML resolution chain, merge logic
- **MCP tools (src/mcp/)**: tool registration, parameter handling
- **Engine (src/engine/)**: review pipeline, scan pipeline, LLM abstraction
- **Pre-commit hook (src/hook/)**: git integration, diff generation

The bar for the test is real coverage of the contract, not a placeholder. Test the edge case that would actually break. If you can't see how to test it, ask in an issue before opening the PR.

## What Cora is not

To set expectations:

- Not trying to be a full IDE plugin or language server (though it could integrate with them).
- Not building: web UI, multi-user collaboration, enterprise SSO.
- Not a curated "first open-source contribution" project. Beginners are welcome but expect normal review.
- Mechanical refactors, broad style changes, drive-by rewrites are not helpful.
- AI-assisted contributions are welcome, but the PR must reflect understanding of the existing patterns. Low-effort AI-generated code that wasn't read by the author will be closed.

## Branches

Branch off `develop`. Use these prefixes (kebab-case):

| Prefix        | Use for                                  |
| ------------- | ---------------------------------------- |
| `feat/`       | New feature                              |
| `fix/`        | Bug fix                                  |
| `chore/`      | Refactor, tooling, config, dependencies  |
| `docs/`       | Docs-only changes                        |
| `perf/`       | Performance work                         |
| `security/`   | Security fix or hardening                |
| `refactor/`   | Code restructuring                       |
| `test/`       | Test additions/changes                   |

Examples: `feat/mcp-find-callers`, `fix/fts5-camelcase`, `security/path-guard`.

Don't open PRs from your fork's `develop` or `main` branch. Work on a feature branch.

## Commits & PRs

The **PR title becomes the squash commit** for most PRs. Title must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(index): add camelCase split for FTS5 queries
fix(review): handle empty diff without panic
chore(deps): bump tree-sitter to 0.24.0
security(rules): tighten secret pattern regex
docs(readme): update installation instructions
```

Types: `feat`, `fix`, `chore`, `docs`, `perf`, `refactor`, `test`, `build`, `ci`, `security`.

Common scopes: `index`, `graph`, `embed`, `schema`, `config`, `mcp`, `engine`, `review`, `scan`, `hook`, `rules`, `profiles`.

**Fill out the PR template.** Include: what changed, why, how you tested. The more specific, the faster the review.

**Open a draft PR early** if you want feedback mid-flight. Mark "Ready for review" when done.

### What gets merged faster

- Clear problem statement
- Small, focused diff
- Follows existing patterns (read 2–3 nearby files before writing yours)
- All checks pass (fmt, clippy, tests)
- Manual testing notes describing the steps you took

### What gets bounced back

- Mixed-concern PRs
- Large architectural PRs without prior discussion
- New dependencies without justification
- Breaking changes without migration notes
- Incidental reformatting unrelated to the change
- AI-generated code that obviously wasn't read by the author

## Code Style

- Follow existing patterns. Read 2–3 adjacent files before adding new ones.
- Rust: `cargo fmt` + `cargo clippy` clean. Clippy warnings are errors in CI.
- Comments: only for *why*, not *what*. Code should explain itself.
- No emojis in code or commit messages.

## Architecture

Cora is a single crate with modular source layout:

| Module | Purpose |
| ------ | ------- |
| `src/main.rs` | CLI entry point + clap args |
| `src/commands/` | CLI subcommands (review, scan, index, config, etc.) |
| `src/engine/` | Review + scan pipeline, LLM abstraction, types |
| `src/index/` | Symbol indexing, FTS5, code graph, schema migration |
| `src/embed/` | ONNX embedding (nomic-embed-code, vendored) |
| `src/config/` | YAML config resolution chain + merge logic |
| `src/mcp/` | MCP server — tool registration + handlers |
| `src/formatters/` | Output formatting (pretty, json, compact, sarif) |
| `src/profiles/` | Quality profiles + rules engine |
| `src/hook/` | Pre-commit hook integration |
| `src/git/` | Git operations (diff, blame, log) |

```
src/
├── main.rs                  # CLI entry point
├── commands/                # CLI subcommands
│   ├── review.rs           # cora review
│   ├── scan.rs             # cora scan
│   ├── index_cmd.rs        # cora index
│   └── ...
├── engine/                  # Core engine
│   ├── review.rs           # LLM review pipeline
│   ├── scanner.rs          # File scanning
│   ├── types.rs            # Issue, Severity, etc.
│   ├── llm.rs              # LLM API abstraction
│   ├── context/            # Context building for review
│   └── rules/              # Rule definitions
├── index/                   # Code intelligence
│   ├── schema.rs           # SQLite schema + migrations
│   ├── symbols.rs          # Symbol extraction + FTS5
│   ├── graph.rs            # Call graph + dead-code detection
│   └── mod.rs              # Index orchestration
├── embed/                   # Embedding engine
│   └── onnx.rs             # ONNX runtime wrapper
├── config/                  # Configuration
│   ├── schema.rs           # YAML structs + merge
│   ├── loader.rs           # Resolution chain
│   └── providers.rs        # LLM provider presets
├── mcp/                     # MCP server
│   └── tools.rs            # Tool handlers
├── formatters/              # Output formatting
├── profiles/                # Quality profiles
├── hook/                    # Pre-commit hooks
└── git/                     # Git operations
```

### Key Design Decisions

- **SQLite-first indexing** — Symbol data, FTS5, and code graph all live in a single `cora.db` per project
- **Tree-sitter for parsing** — Language-agnostic AST extraction for symbol indexing
- **Vendored nomic-embed-code** — No external embedding API dependency; ONNX runtime bundled
- **Schema versioning** — Integer counter; auto-migration on upgrade (current: v6)
- **Config merge chain** — Global → project `.cora.yaml` → CLI flags, with profile overlay
- **Pre-commit hook** — Cora runs as a git pre-commit hook reviewing staged diff via LLM

## Release flow

Cora follows a simplified GitFlow:

```
develop (default) → PR → main → tag vX.Y.Z → release workflow
```

- Tags must be on `main`, not `develop`.
- The release workflow verifies tag ancestry to `origin/main` before building.
- Version bumps happen on `develop` before syncing to `main`.

## Reporting Issues

- **Bugs:** Use the [Bug Report](https://github.com/codecoradev/cora-code/issues/new?template=bug_report.yml) template
- **Features:** Use the [Feature Request](https://github.com/codecoradev/cora-code/issues/new?template=feature_request.yml) template

## FAQ

**Q: Should I ask before fixing a typo or obvious bug?**
A: No, open a PR directly.

**Q: I have an idea for a new feature.**
A: Open a GitHub issue. Don't open a PR without prior discussion.

**Q: My PR was closed without detailed feedback.**
A: Usually means it didn't align with project direction, or scope was too large to review responsibly. This is normal for a solo project.

**Q: Can I work on an open issue?**
A: Comment first to confirm it's still relevant. For anything non-trivial, discuss approach before implementing.

**Q: My PR conflicts after develop moved. Should I rebase?**
A: If the change is still relevant and reasonably small, yes. Large stale PRs may be closed with an offer to reopen after rebase.

**Q: I don't have an LLM API key. Can I still contribute?**
A: Yes. Code changes to non-LLM paths (index, graph, config, formatting) don't require an API key. Tests run without one. Only `cora review` needs a provider configured.

## Security issues

Don't file them as public issues. See [SECURITY.md](SECURITY.md).

## Code of Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

By contributing you agree your work is licensed under [MIT](LICENSE). No CLA required.
