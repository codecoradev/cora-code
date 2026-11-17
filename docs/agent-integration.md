# AI Agent Integration

An agent only searches your code when the model remembers it has a tool for it. `cora install` removes the guesswork: for each agent it adds a **prompt hook** (cora's search results are put in front of the model before it answers), the **MCP server** (so the model can go further: callers, impact, affected tests) and a short **skill**.

![The agent sends your prompt through the cora hook, which adds matching code from the index; the model answers, and can call the cora MCP server for deeper questions.](/diagrams/agent-flow.svg)

```bash
cora install --dry-run          # show what would change
cora install                    # set up every detected agent
cora install --agents claude,hermes
cora install --remove           # undo exactly what cora wrote
cora install --no-instructions  # leave CLAUDE.md alone
```

Run `cora index` in your project first. The hook is silent for chit-chat and for projects that are not indexed, never downloads anything, always exits 0 and has a hard timeout, so it cannot slow down or block an agent.

## What each agent gets

| Agent | Prompt hook | MCP | Skill |
|---|---|---|---|
| Claude Code | `UserPromptSubmit` and `SessionStart` in `~/.claude/settings.json` | `claude mcp add-json` with `alwaysLoad` | `~/.claude/skills/cora-code/` plus a block in `~/.claude/CLAUDE.md` |
| Hermes | `hooks.pre_llm_call` in `~/.hermes/config.yaml` (appended to your message) | `mcp_servers.cora` | `~/.hermes/skills/` |
| pi | `before_agent_start` extension in `~/.pi/agent/extensions/` | `~/.pi/agent/mcp.json` with `exposure: direct` | `~/.pi/agent/skills/` |
| omp | `before_agent_start` extension in `~/.omp/agent/extensions/` | `~/.omp/agent/mcp.json` | `~/.omp/agent/skills/` |
| opencode | `chat.message` plugin in `~/.config/opencode/plugins/` | `mcp.cora` in `opencode.json` | `~/.config/opencode/skills/` |

Everything is written under your home directory, never inside a project.

## Safe by construction

- Idempotent: a second run changes nothing. `--dry-run` writes nothing.
- A file of yours is never replaced: cora marks the files it writes, keeps a one-time `.cora-backup` of a config it edits, refuses a config it cannot read or parse, and does not rewrite `opencode.jsonc` (comments) or a `hooks:` / `mcp_servers:` key you already have in Hermes (it prints the snippet instead).
- `--remove` deletes only what cora wrote and restores your files.

## Skills stay in step with cora

The skills (`cora-code`, and `cora-review` for Claude Code) follow the Agent Skills layout: a short `SKILL.md` plus `references/` files the agent loads only when needed (`ignore.md`, `tools.md`, `scan.md`, `ci.md`). They teach the agent to set up ignores before the first review or scan, to review the whole branch with cora before opening a pull request, and to suggest `cora review` in CI. They ship inside the binary and carry a marker with the cora version that wrote them. After an upgrade, `cora install` refreshes the copies cora wrote; a skill you edited is kept (use `--force` to replace it). The build fails if a skill names an MCP tool, command or flag that cora does not have.

## Things to know

- **Hermes** asks once to approve the new hook (or set `HERMES_ACCEPT_HOOKS=1`). Its MCP support needs the `mcp` Python package.
- **Claude Code headless** (`claude -p`) asks before using MCP tools; allow the read-only ones with `permissions.allow` (`mcp__cora__cora_brain_search`, `cora_search_symbols`, `cora_find_callers`, `cora_find_impact`, `cora_find_affected_tests`). `cora install` prints the hint and does not edit your permissions.
- **opencode**: the plugin targets the plugin API of opencode 1.x (`chat.message`).
- Tested for real (isolated home, fake provider key so no model cost) with Claude Code 2.1.325, pi 1.5.2, omp 18.21.0, Hermes 0.19.0 and opencode 1.19.5.

## `cora hook context`

The command the hooks call. You can use it from any agent that has a shell hook:

```bash
cora hook context --prompt "where do we retry a failed request"   # plain text
cora hook context --agent claude    # reads Claude Code's hook JSON on stdin
cora hook context --agent hermes    # reads Hermes' hook JSON, answers {"context": "..."}
```

`cora hook session` prints a one-line reminder at session start. Both read `--max-chars`, `--limit` and `--budget` (seconds) options; see `cora hook context --help`.
