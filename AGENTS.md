# AGENTS.md — cora-code

Rust CLI for code review and code intelligence (BYOK). The module map and
code structure are in `AGENT.md`.

## Build & test

- `cargo build`, then `cargo test --features tree-sitter`.
- CI runs the same gates: `cargo fmt --all -- --check` and
  `cargo clippy --all-targets --features tree-sitter -- -D warnings`.
- User-visible changes go under `## [Unreleased]` in `CHANGELOG.md`.
- Before committing: review the staged diff (`cora review --staged`, or the
  `/cora-review` skill) and run `scripts/setup-hooks.sh` once per clone so the
  pre-commit hook enforces the same gates. Never `--no-verify` past a finding.

## Conventions enforced by CI

- Branch names start with one of `feat/ fix/ docs/ chore/ perf/ security/
  refactor/ test/ build/ ci/ release/`.
- PR bodies contain `## What`, `## Why` and `## Testing`.
- Conventional Commits; the PR title becomes the squash message.

## Gotchas

- The pre-commit hook runs the installed `cora` binary, not
  `target/debug/cora`; a stale installed build can disagree with the working
  tree.
- Rule and scanner changes: measure on a real project before the PR (unit
  fixtures alone hide false positives), and keep a regression test for each
  false positive or bypass found.
- Never print or log credentials, including environment values in command
  output.

## Session start (continuity ritual)

- New session: read & understand the team knowledge rooms before any work.
- Save every decision or important outcome back to the responsible room
  during or at the end of the session.
- Handing over mid-task: leave a [HANDOFF] entry (state, next step, blockers).

## Workflow summary

Issue → branch from develop → code + pre-commit checks → push → PR to develop
(## What / ## Why / ## Testing, English) → CI green + review verdict → merge →
cleanup → release (bump + CHANGELOG + tag). 1 issue = 1 branch = 1 PR.
