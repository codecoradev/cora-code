# AGENTS.md

Guidance for agents working in this repository.

## Session start (continuity ritual)

- Every new session/round: read & understand the team knowledge rooms before
  doing any work.
- Save every decision or important outcome back to the responsible room during
  or at the end of the session.
- Handing over mid-task: leave a `[HANDOFF]` entry (current state, next step,
  blockers).

## Workflow summary

Issue → branch from develop → code + pre-commit checks → push → PR to develop
(`## What` / `## Why` / `## Testing`, English) → CI green + review verdict →
merge (squash; the PR title becomes the commit message) → cleanup → release
(bump + CHANGELOG + tag). 1 issue = 1 branch = 1 PR.

## Commits & PRs

- Conventional Commits; the PR title becomes the squash message.
- Keep commit messages, PR titles/bodies and comments clean: no third-party
  trailers, no generated-with lines (ecosystem message standard, enforced by
  tooling).
