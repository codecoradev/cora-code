# cora agent-integration research (read-only; docs scraped 2026-11-16)

Legend: [V] = read in the cited doc; [I] = inference from docs (flagged); the URL after each block is the source.
All web content treated as data; nothing was executed or installed.

Cross-agent headline findings (read these first):
1. **MCP tools are hidden/deferred by default in 4 of 5 agents.** Registering `cora mcp` is not enough:
   - Claude Code: tool search is the default, MCP tools are deferred; per-server `"alwaysLoad": true` exempts (needs v2.1.285+).
   - opencode (V2 docs): `codemode` defaults to true, tools live under `tools.<server>.<tool>(...)` via Code Mode; set `"codemode": false`.
   - pi (main docs): `exposure` defaults to `codemode` (not declared to model); set `"exposure": "direct"`.
   - omp: MCP tools are bridged as `mcp__...`; "DeferredMCPTool" exists; whether they are listed to the model by default is NOT verified.
   - Hermes: no hiding by default (all tools of a configured server registered; filter only via `tools.include/exclude`).
2. **pi now HAS built-in MCP** (`pi mcp add`, `~/.pi/agent/mcp.json`, built-in extension `builtin:mcp`). The old "pi has no MCP" is outdated for current main docs (earendil-works/pi). Older installs may lack it: [unverified which version introduced it].
3. **opencode docs are V2** (`plugins`, `mcp.servers`, `permissions` arrays, `Plugin.define`). The installed version may be V1 (`plugin`, `mcp.<name>`, `permission`, `export const X: Plugin = async ()=>({...})`). The docs say V1 object entrypoints are supported from 1.18.29; a dual export is documented. Installer must detect the version.
4. The strongest "before the model works" hooks: Claude `UserPromptSubmit` (command stdout), Hermes `pre_llm_call` (shell hook JSON `{"context":...}`), pi `before_agent_start` / `input`, omp `before_agent_start` / `input`, opencode V2 `session.hook("prompt")` / `"context"`.

---------------------------------------------------------------------------------------------------

## 1. Claude Code

### (a) MCP
- Scopes: local = `~/.claude.json` (per-project entry), project = `.mcp.json` in project root (shared via VCS), user = `~/.claude.json`. Precedence local > project > user > plugin > claude.ai. https://code.claude.com/docs/en/mcp
- CLI: `claude mcp add [--scope local|project|user] <name> -- <cmd> <args>`; `claude mcp add-json <name> '<json>'`.
- Project `.mcp.json` shape (stdio):
```json
{ "mcpServers": { "cora": { "type": "stdio", "command": "cora", "args": ["mcp"], "alwaysLoad": true } } }
```
  `alwaysLoad` listed among per-server fields; "tool loads upfront" semantics and v2.1.285 requirement [V, mcp doc "Exempt a server from deferral"]. Exact placement of `alwaysLoad` in a stdio entry: [I] (doc's code blocks were stripped by the scraper; field named in the WebSocket/HTTP paragraph).
- Project-scoped servers need user approval first run ("Pending approval" in `claude mcp list`; `disabledMcpjsonServers` rejects). So an installer writing `.mcp.json` leaves the server unapproved until the user runs `claude`. Prefer `--scope user` or `local` for zero-prompt. [V]
- Tool names: `mcp__<server>__<tool>`; plugin servers `mcp__plugin_<plugin>_<server>__<tool>`; hook matcher can use `mcp__cora__.*`. [V]
- Env expansion in `.mcp.json`: `${VAR}` in command/args/env/url. [V]

### (b) Skills
- Personal `~/.claude/skills/<name>/SKILL.md`; project `.claude/skills/<name>/SKILL.md`; nested `<subdir>/.claude/skills/...`; plugin; enterprise. https://code.claude.com/docs/en/skills
- Frontmatter: `name`, `description` (+ `when_to_use`, combined cap 1,536 chars in listing), `disable-model-invocation`, `allowed-tools`, `argument-hint`, `context: fork`, etc. [V]
- Activation: model-driven by description in skill listing (Skill tool), or user `/skill-name`. `disable-model-invocation: true` removes model autonomy. [V]

### (c) Hooks (already verified by caller; additions only)
- Hook matcher can match MCP tools (`mcp__server__tool`). [V per caller]
- No hook can add a persistent system-prompt fragment; UserPromptSubmit/SessionStart `additionalContext` is turn/session context. [I, consistent with caller notes]

### (d) Instruction files
- `CLAUDE.md` (project root and parents, `~/.claude/CLAUDE.md`, `.claude/CLAUDE.md`, nested subdir CLAUDE.md load on demand); `@import` supported. https://code.claude.com/docs/en/skills (mentions CLAUDE.md "always-loaded guidance ... nested CLAUDE.md files that load on demand"). Full memory doc not scraped [unverified details].
- Managed block: no doc statement against tool-appended blocks; safe via markers `<!-- cora:begin -->...<!-- cora:end -->` [I].

### (e) Surprises
- Tool search default defers MCP tool schemas; model finds cora tools via ToolSearch unless `alwaysLoad`. [V]
- `~/.claude/skills` is also read by opencode (compat) and omp (opt-in, see below): a skill installed here is seen by 3 agents.

### Most reliable "search before edit"
UserPromptSubmit command hook (deterministic, runs even if model never chooses a tool) + short CLAUDE.md rule + `alwaysLoad` MCP. Skill is weakest (description-matching only). [I]

---------------------------------------------------------------------------------------------------

## 2. Hermes Agent (Nous Research)

### (a) MCP
- Global only, `~/.hermes/config.yaml` (profiles: `~/.hermes/profiles/<name>/`) under `mcp_servers`. No project-level MCP file found in docs [unverified absence]. https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp
```yaml
mcp_servers:
  cora:
    command: "cora"
    args: ["mcp"]
    # cwd: optional; default = session working dir when pinned, else Hermes process dir
    # tools: { include: [...], exclude: [...], prompts: false, resources: false }
    # timeout, connect_timeout (s), lazy: false, enabled: true
```
- CLI: `hermes mcp add <name> ...`, `hermes mcp install <catalog-name>`, `hermes mcp configure`, `hermes mcp test <name>`, `hermes mcp login`. [V]
- Tool names: doc says `mcp_<server>_<tool>` (non-alnum -> `_`, e.g. `my-api`+`query.data` -> `mcp_my_api_query_data`). The toolsets reference instead writes `mcp__<server>__*`. **Contradiction between two Hermes pages; treat exact prefix as unverified.** https://hermes-agent.nousresearch.com/docs/reference/toolsets-reference
- stdio servers get only configured `env` + a safe baseline, NOT the full shell env (so PATH/HOME-dependent cora config must be passed in `env:`). [V]
- Config reload: CLI auto-reloads MCP connections in-session (30 s timeout). [V]

### (b) Skills
- Primary `~/.hermes/skills/<category>/<name>/SKILL.md`; external dirs via `skills.external_dirs: [~/.agents/skills, ...]` in config.yaml; project-local `<git-root>/.hermes/skills/` and `<git-root>/.agents/skills/` but ONLY after `hermes skills trust` (stored in `skills.trusted_project_dirs`); precedence project > local > create_dir > external. https://hermes-agent.nousresearch.com/docs/user-guide/features/skills
- Frontmatter: `name`, `description`, `version`, `platforms`, `metadata.hermes.{tags,category,fallback_for_toolsets,requires_toolsets,config}`, `required_environment_variables`. Agentskills.io compatible. [V]
- Activation: progressive disclosure: `skills_list()` (name+description in index/system prompt), model calls `skill_view(name)`; also every skill is a `/skill-name` slash command. Skills with `requires_toolsets: [terminal]` appear only if toolset present. [V]
- Agent can modify/delete any skill in `~/.hermes/skills` (an installed cora skill may be rewritten by the curator) [V].

### (c) Hooks
Three mechanisms (comparison table in https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks):
1. **Shell hooks** (`hooks:` in `~/.hermes/config.yaml`) - best fit for `cora hook context`.
```yaml
hooks:
  pre_llm_call:
    - command: "~/.hermes/agent-hooks/cora-context.sh"   # shlex.split, shell=False
      timeout: 15            # default 60, capped 300
hooks_auto_accept: false
```
   - Wire protocol: Hermes pipes JSON on stdin (`hook_event_name`, `session_id`, `cwd`, `profile`, `extra.{user_message,conversation_history,...}`, `tool_name/tool_input` null for non-tool events) and reads stdout JSON. Context injection: stdout `{"context": "text"}`; `{}` for none. [V, example script `jq --null-input ... '{context: ...}'`]
   - `pre_llm_call`: fires once per user turn, before the tool loop; context is appended to the CURRENT USER MESSAGE (never the system prompt, to keep prompt cache); multiple hooks joined with blank lines. Can also veto turn with `{"action":"block","message":...}`. [V]
   - Failure: spawn error/timeout/bad JSON/non-zero = log warning, proceed (fail-open); `fail_closed` only for `pre_tool_call`. Exit code 2 blocks only `pre_tool_call`. [V]
   - **Consent**: each unique `(event, command)` prompts first use (foreground `hermes chat` only); stored in `~/.hermes/shell-hooks-allowlist.json`; bypass with `--accept-hooks`, `HERMES_ACCEPT_HOOKS=1`, or `hooks_auto_accept: true`. Gateway/cron/acp never prompt and silently leave un-approved hooks unregistered. An installer must tell the user to approve (or pre-write the allowlist: format of that JSON is [unverified]). `hermes hooks list|test|revoke|doctor`. [V]
2. **Python plugin hooks** (`~/.hermes/plugins/<name>/` + `plugin.yaml`, `register(ctx)`; `ctx.register_hook("pre_llm_call", fn)`; enabled via `plugins.enabled`). `pre_llm_call` callback returns str / `{"context":...}` / None; timeout `plugins.hook_callback_timeout` 30 s default. [V; plugin.yaml schema not read]
3. Persistent system-prompt fragment: only via Python plugin `ctx.register_system_prompt_section(id, str|callable, position="after_memory", max_chars<=4000)`; total 8,000 chars / 32 sections; rendered once per new session. [V]
- Other events: `on_session_start` (observer, return ignored), `pre_tool_call`, `post_tool_call`, `post_llm_call`, `on_session_end`, `subagent_stop`; gateway hooks (`~/.hermes/hooks/<n>/HOOK.yaml`+`handler.py`) cannot inject context. [V]

### (d) Context files
- Priority, first match wins (ONE project type per session): `.hermes.md`/`HERMES.md` > `AGENTS.override.md` > `AGENTS.md` > `CLAUDE.md` > `.cursorrules`. AGENTS.md chain from git root to cwd (deduped), subdirectory AGENTS.md loaded progressively; outside git only cwd. `SOUL.md` global identity (`~/.hermes/SOUL.md`). https://hermes-agent.nousresearch.com/docs/user-guide/features/context-files
- Cap `context_file_max_chars` (dynamic, floor 20,000), head/tail truncation 70/20.
- **Security scanner blocks a file entirely** if it matches injection patterns ("ignore previous instructions", "do not tell the user", curl with $API_KEY, `cat .env`, hidden HTML comments, zero-width chars). A managed block must avoid such phrases and `<!-- -->` comments hiding instructions (HTML comment markers for the managed block are a risk: "Hidden HTML comments: <!-- ignore instructions -->" - pattern details unverified; use visible marker lines instead). [V/I]
- Because only the first match loads: if the project has `.hermes.md`, a cora block in AGENTS.md is ignored. Writing to whichever file is the winner is required.

### (e) Surprises
- Toolsets: each MCP server gets toolset `mcp-<server>` (alias `<server>`); in `platform_toolsets.<platform>` a list naming no MCP server gets all enabled servers, a list naming some gets only those, `no_mcp` drops all, `email: []` gets none. `hermes tools` UI can disable individual tools persistently. https://hermes-agent.nousresearch.com/docs/reference/toolsets-reference
- Server named like a built-in toolset (`browser`, `web`) resolves to the built-in. Name the server `cora` (safe).
- Project skills need trust; hooks need consent; `hermes mcp-serve` etc never prompt.
- Hermes can import Claude Code config: `hermes import-agent claude-code` migrates `mcpServers`, skills, instructions (agent-importers plugin). [V, MCP doc]

### Most reliable "search before edit"
`pre_llm_call` shell hook injecting results (deterministic, cache-safe) + managed block in the winning context file. Skills/MCP alone depend on model choice. [I]

---------------------------------------------------------------------------------------------------

## 3. omp (oh-my-pi, can1357)

### (a) MCP
- Native: project `.omp/mcp.json`; user `~/.omp/agent/mcp.json` (profile: `~/.omp/profiles/<p>/agent/mcp.json`; `PI_CONFIG_DIR`, `PI_CODING_AGENT_DIR` relocate). Also reads `.omp/.mcp.json`, root `mcp.json`/`.mcp.json`, and imports Claude (`~/.claude.json`, `.claude/.mcp.json`), Codex TOML, Gemini, opencode.json(c), Cursor, Windsurf, `.vscode/mcp.json`. Priority: omp native > extension packages > Claude Code > ... > root mcp.json; first definition wins, no merge. https://raw.githubusercontent.com/can1357/oh-my-pi/main/docs/mcp-config.md
```json
{ "$schema": "https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/config/mcp-schema.json",
  "mcpServers": { "cora": { "type": "stdio", "command": "cora", "args": ["mcp"], "timeout": 30000, "instructions": false } } }
```
  Fields: `type` stdio|http|sse, `command,args,env,cwd`, `enabled`, `timeout` ms (default 30 s, 0 disables), `instructions` (default true: server instructions appended to system prompt, each capped 4,000 chars), `auth`, `oauth`. `${VAR}`/`${VAR:-d}` expansion; env values starting `!` run a command. [V]
- Tool names: `mcp_<server>_<tool>` per the architecture diagram, but the rules text says `mcp__<server>_<tool>`-like pattern (placeholders were stripped by the scraper); prefix `mcp__` appears in the intro ("callable `mcp__*` tools"), lowercased, non `[a-z0-9_]` -> `_`, >64 chars hashed. Use `mcp__cora_<tool>` as best guess; **exact separator unverified**. https://raw.githubusercontent.com/can1357/oh-my-pi/main/docs/mcp-server-tool-authoring.md
- Quirks: browser-like server names (`playwright`, `browser`, ...) silently dropped; first turn waits up to 1.5 s for MCP startup (interactive), so first-turn system prompt may lack slow servers; print mode waits up to 30 s. `/mcp list|add|reload|test`. [V]
- `mcp.discoveryMode` settings are listed as "removed" in settings.md; whether MCP tools are visible directly or deferred by default: [unverified].

### (b) Skills
- Native providers `.omp` user/project skills (path form `~/.omp/agent/skills/<name>/SKILL.md`, `.omp/skills/<name>/SKILL.md` [I from "native .omp user/project skills", exact dir name not printed]); also Claude (`.claude/skills`), Codex, `.agents/skills`/`.agent/skills`, opencode, `.github/skills`, plugins; `skills.customDirectories`. Non-recursive: exactly `<root>/skills/<name>/SKILL.md`. https://raw.githubusercontent.com/can1357/oh-my-pi/main/docs/skills.md
- **Foreign USER-level sources (e.g. `~/.claude/skills`) are opt-in via `enabledProviders`; project-level foreign roots load by default.** (context-files.md) So `~/.claude/skills` is NOT automatically seen by omp. Native and `.agent/.agents` user sources are on by default.
- Frontmatter: `name` (defaults to dir), `description` (required for native provider), `globs`, `alwaysApply` (metadata only), `hide`, `disable-model-invocation`, `enabled: false`.
- Activation: name+description in system prompt (only if an active tool declares `readsSkillUris`, i.e. `read`); model reads `skill://<name>`; `/skill:<name>` command if `skills.enableSkillCommands`. Collisions: higher-priority keeps bare name, differing copies get `<namespace>/name`; identical copies collapse silently.

### (c) Hooks / extensions
- Extensions = TS modules `export default function(pi: ExtensionAPI)`; discovered from `.omp/extensions/` (project, cwd only), `~/.omp/agent/extensions/`, `extensions:` in `config.yml`/`settings.json`, `--extension/-e`; hooks (legacy, same `pi.on` API) from `.omp/hooks/pre|post/*.ts` and `~/.omp/agent/hooks/pre|post/*.ts` (files directly in `hooks/` NOT discovered). https://raw.githubusercontent.com/can1357/oh-my-pi/main/docs/extension-loading.md , .../hooks.md
- Does NOT read Claude `settings.json` hooks [unverified; docs describe only the TS hook capability].
- Events (prompt lifecycle): `input` (ingress, before command/skill/template expansion; can return `{text}` transform or `{handled:true}`), `before_agent_start` (per user prompt/steer batch; can return a custom `message` appended after the batch and a system-prompt override; first returned message kept in legacy path), `context` (messages transform per call), `before_provider_request`, `agent_start/end`, `turn_*`, `session_start`, `session_stop`, `tool_call` (can return `additionalContext` delivered after tool results with developer/system priority; `block`; `input`), `tool_result` (`additionalContext`), `session.compacting`, `session_shutdown`, `mcp_notification`, etc. [V]
- Shell: `pi.exec(cmd, args, opts)` available even during load; capture stdout/code. [V in extensions.md]
- Context injection snippet [I, composed from documented shapes]:
```ts
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
export default function (pi: ExtensionAPI) {
  pi.on("before_agent_start", async (event) => {
    const r = await pi.exec("cora", ["hook", "context", "--prompt", event.prompt ?? ""], { timeout: 15000 });
    if (r.code !== 0 || !r.stdout.trim()) return;
    return { message: { customType: "cora", content: r.stdout, display: false } };
  });
}
```
  (`display`/`attribution` field requirements of `message` not fully verified.)
- Timeouts: most extension handlers 30 s budget; `tool_call` 30 s default (`extensionHandlers.toolCallTimeoutMs`), fail closed; other handler errors are caught and logged, session continues. [V]
- Persistent fragments: `RULES.md` (native) is always-apply, carried on every request; `SYSTEM.md`/append text in system-prompt-customization.md (not read).

### (d) Context files
- Native `~/.omp/agent/AGENTS.md` (user) and `<repo>/.omp/AGENTS.md` (nearest non-empty `.omp/` only; no walk past it); `~/.omp/agent/RULES.md` and `.omp/RULES.md` sticky rules; plus `CLAUDE.md`/`AGENTS.md` (standalone, walk up to repo root), `.claude/CLAUDE.md`, `.gemini/GEMINI.md`, `.github/copilot-instructions.md`, `contextFiles.extra`. One user file survives (native wins and SHADOWS ~/.claude/CLAUDE.md etc.); one project file per depth (higher-priority provider shadows the rest at same depth). https://raw.githubusercontent.com/can1357/oh-my-pi/main/docs/context-files.md
- **Task subagents drop every file named AGENTS.md but keep RULES.md.**
- `@path` imports expanded inline.
- Writing a managed block into an existing AGENTS.md/CLAUDE.md works (plain Markdown); no doc prohibits it. Creating `~/.omp/agent/AGENTS.md` where none exists would SHADOW the user's `~/.claude/CLAUDE.md`/others: do not create it blindly.

### (e) Surprises
- `.omp` vs `.claude`: native `.omp` has highest priority and shadows; user-level foreign opt-in; `disabledProviders: [claude]` kills ALL Claude-sourced things (MCP, skills, hooks), not just CLAUDE.md. `omp --no-extensions`, `--trusted-extension` exist; project inputs (extensions, hooks, MCP) load by design on opening a repo (trust model: no prompt).
- Foreign project `.mcp.json` loads by default; `mcp.enableProjectConfig: false` disables.
- Installing cora hook into `~/.omp/agent/extensions/` is user-global; `.omp/extensions/` is cwd only (no ancestor walk).

### Most reliable
Extension `before_agent_start`/`input` injection + sticky `RULES.md` (survives long sessions, reaches subagents) + MCP. [I]

---------------------------------------------------------------------------------------------------

## 4. pi (earendil-works/pi, formerly badlogic/pi-mono)

### (a) MCP (built-in on current main)
- User `~/.pi/agent/mcp.json`, project `.pi/mcp.json` (read only after project trust); project entry replaces same-name user entry. `pi mcp add [--local|-l] <name> -- <cmd> args`, `pi mcp list|remove|login|logout`, in-session `/mcp`, `/reload`. https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/mcp.md
```json
{ "mcpServers": { "cora": { "command": "cora", "args": ["mcp"], "exposure": "direct",
    "description": "Semantic code search, callers and impact analysis", "timeout": 60 } } }
```
  `command`+`args` (no shell string), `env` (`${VAR}`, `!cmd`), `cwd`, `timeout` seconds (default 60), `enabled`, `exposure`, `toolExposure`, `description`. Server names: letters, digits, `_`, `-`.
- Tool names: `mcp__<server>__<tool>`, chars other than letters/digits/`_` -> `_` (hash suffix on collisions). [V]
- **Exposure default `codemode`**: tools are NOT declared to the model, reachable only through the `codemode` script tool; `deferred` via `tool_search`; `direct` declares like built-in; `hidden`. Per-tool override `toolExposure: {"search_*": "direct"}`. [V] Servers with codemode/deferred are listed in the `mcp_servers` section of the system prompt with `description`.
- First prompt waits up to 10 s only for servers with `direct` tools. [V]
- Extensions can register servers per session: `pi.registerMcpServer("cora", {command:"cora", args:["mcp"], exposure:"direct"})` (same shape; not persisted; a mcp.json entry of same name wins). https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/extensions.md
- An extension that registers `/mcp` (e.g. `pi-mcp-adapter`) REPLACES the built-in MCP support entirely (mcp.json then not read by pi). `--no-mcp` disables for a run; `"extensions": ["-builtin:mcp"]` in settings disables. [V]
- Older pi without built-in MCP: [unverified] - `pi-mcp-adapter` exists per the docs sentence.

### (b) Skills
- User `~/.pi/agent/skills/`, project `.pi/skills/`, plus Agent Skills locations `~/.agents/skills/` and project `.agents/skills/` (walked from cwd up to repo root); recursive discovery of `SKILL.md`; extra paths via settings/packages/extension `resources_discover`. Project `.pi` and `.agents/skills` need project trust. https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/skills.md , .../configuration.md
- Reading `~/.claude/skills`: not documented [unverified].
- Frontmatter (Agent Skills spec): `name` (<=64, lowercase/hyphen), `description` (<=1024, required, else not loaded), `license`, `compatibility`, `metadata`, `allowed-tools`, `disable-model-invocation`.
- Activation: name+description+path in system prompt; model reads SKILL.md when the task matches; user `/skill:name [args]` forces. Name collision keeps first discovered + warning. `/reload` after edits.

### (c) Extensions / hooks
- TS modules, default export `function (pi: ExtensionAPI)`; locations `~/.pi/agent/extensions/*.ts|*/index.ts` (global), `.pi/extensions/` (project, only after trust), `settings.json` `extensions`/`packages`, `pi -e path.ts`. `/reload` hot reload.
- Lifecycle: `project_trust`, `session_start{reason}`, `resources_discover`, `input` (before skill/template expansion; return `{action:"continue"|"transform",text}|{action:"handled"}`), **`before_agent_start`** (after input handling and expansion, before the run; return `{message:{customType,content,display}, systemPrompt?}`; mutate `event.systemPromptOptions.{sections,promptGuidelines,appendSystemPrompt,selectedTools}`), `agent_start`, `turn_start`, `context`, `context_with_system`, `before_provider_request`, `tool_call` (block/mutate), `tool_result`, `tool_execution_*`, `agent_end`, `agent_settled`, `session_before_compact`, `session_shutdown`, `mcp_servers_change`, ... [V]
- `before_agent_start` `message` is "persistent message (stored in session, sent to LLM)"; `systemPrompt` replacement is per-turn/chained; preferred route = change `sections`/`promptGuidelines` (diffed, appended as one system message patch). Models that cannot take mid-conversation system messages get a cache miss once per change. [V]
- Shell: `await pi.exec("cora", ["hook","context", ...], {signal, timeout: 15000})` (no shell; returns `{stdout,stderr,code,killed}`; SIGKILL after 5 s). [V]
```ts
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
export default function (pi: ExtensionAPI) {
  pi.on("before_agent_start", async (event, ctx) => {
    if (event.messageType !== "user") return;
    const r = await pi.exec("cora", ["hook", "context", "--prompt", event.prompt ?? ""], { timeout: 15000 });
    if (r.code !== 0 || !r.stdout.trim()) return;
    return { message: { customType: "cora-context", content: r.stdout, display: false } };
  });
}
```
  (`display:false` allowed: [I]; docs example uses `display: true`.)
- Errors: extension errors are logged and the agent continues; `tool_call` errors block the tool (fail-safe). Handlers run in load order. [V]
- Mode: print/JSON modes run extensions but `ctx.hasUI=false`. [V]

### (d) Instruction files
- Agent dir `~/.pi/agent/`: `AGENTS.override.md` | `AGENTS.md` | `AGENTS.MD` | `CLAUDE.md` | `CLAUDE.MD`; plus the same in cwd and parents (applies below its directory); `AGENTS.override.md` replaces AGENTS/CLAUDE only in the same directory. Context-file discovery does not require trust. `~/.pi/agent/SYSTEM.md` (replace), `APPEND_SYSTEM.md` (append) and `.pi/SYSTEM.md`, `.pi/APPEND_SYSTEM.md` (project wins over user, not merged). https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/docs/configuration.md
- A managed block in AGENTS.md/CLAUDE.md or a dedicated `.pi/APPEND_SYSTEM.md` works; `APPEND_SYSTEM.md` project file would override (not combine with) an existing user one [V]; avoid clobbering an existing file.

### (e) Surprises
- Default `codemode` exposure (see a). Package namespace moved: `@earendil-works/pi-coding-agent` (legacy `@mariozechner/*` import names are mapped, per extensions.md and omp's loader).
- Project resources (`.pi/*`, `.agents/skills`) require trust, so a project-level installed extension is inert until trusted; user-level install is safer. `pi mcp` shell commands do not load extensions.
- No permission system by default; tools run with process OS permissions (security.md, not read).

### Most reliable
`before_agent_start` extension injecting message (+ `promptGuidelines`/`sections` for a standing rule) + `exposure: "direct"` MCP. Skill is advisory. [I]

---------------------------------------------------------------------------------------------------

## 5. opencode (opencode.ai, sst)

Docs at opencode.ai describe **V2**. V1 keys differ (see Surprises).

### (a) MCP
- Config files (lowest to highest): `~/.config/opencode/opencode.json(c)`, `<project>/opencode.json(c)` (searched up to root, farthest first), `<project>/.opencode/opencode.json(c)`; managed `/etc/opencode/opencode.json`. https://opencode.ai/docs/config
- V2 shape (note `mcp.servers`, `command` is an ARRAY):
```jsonc
{ "$schema": "https://opencode.ai/config.json",
  "mcp": { "servers": { "cora": { "type": "local", "command": ["cora", "mcp"], "codemode": false,
        "environment": { }, "timeout": { "startup": 30000, "execution": 600000 } } } } }
```
  Fields: `type: local|remote`, `command[]`, `cwd`, `environment` (`{env:NAME}` substitution), `disabled`, `codemode` (default true), `timeout{startup,catalog,execution}`, `protocol`, `pool`. CLI: `opencode mcp add <name> [--global] -- <cmd...>`, `opencode mcp list`. A higher-precedence config REPLACES the whole same-name server object (no field merge). https://opencode.ai/docs/mcp-servers
- Tool names: `<server>_<tool>` with non `[A-Za-z0-9_-]` -> `_`; permission action name is the same; MCP prompts become commands `<server>:<prompt>`. Under Code Mode (default) tools are called as `tools.<server>.<tool>(...)` via an `execute` tool. [V]
- Server-returned `instructions` are added to the system prompt in `<mcp_instructions>`; omitted when permissions deny all that server's tools. [V]
- V1 (older installs) shape [I from migration guide + omp/pi docs]: top-level `mcp: { "cora": { "type": "local", "command": ["cora","mcp"], "enabled": true } }`; pi's mcp.md independently shows opencode V1 uses `environment` and `{env:NAME}`.

### (b) Skills
- Global `~/.config/opencode/skills`, compat `~/.claude/skills`, `~/.agents/skills`; project `.opencode/skills`, `.claude/skills`, `.agents/skills` (searched cwd up to project root); extra via `skills: [paths/URLs]` in config. Forms: `skills/<id>/SKILL.md` or flat `skills/<id>.md`. Disable Claude compat with `OPENCODE_DISABLE_CLAUDE_CODE=1` / `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS=1`. https://opencode.ai/docs/skills , https://opencode.ai/docs/rules
- ID from file path (case-sensitive), frontmatter `name` is only a display label; `description` required to be advertised; `metadata.opencode/autoinvoke: false` or `disable-model-invocation: true` hides from model list. Same ID in two sources: LATER source wins (precedence low->high: built-in, `.claude/skills`, `.agents/skills`, `~/.config/opencode/skills`, project `.opencode/skills`, config entries).
- Activation: each step lists permitted skills (id, name, description); model calls `skill` tool with `{id}`; user types `@skill-id`. Permission action `skill` with resource = id.

### (c) Plugins / hooks
- Location: `.opencode/plugins/*.ts|js` or package dirs (project), `~/.config/opencode/plugins/` (global), `/etc/opencode/plugins/`, or `plugins: [...]` in opencode.json(c) (config entries must be DIRECTORIES). V1's `.opencode/plugin/` also discovered. A `plugins/` dir beside root opencode.json is NOT auto-discovered. https://opencode.ai/docs/plugins , https://opencode.ai/docs/build/plugins
- V2 server plugin: `Plugin.define({ id, setup(ctx) })` from `@opencode/plugin`; `id` required and stable. Hooks registered via `ctx.<domain>.hook(name, cb)`. Hooks run in plugin order; mutable event objects.
- **Before the model sees a prompt**:
  - `ctx.session.hook("prompt", e => {...})`: runs once at admission, before attachment/skill resolution; mutate `e.prompt.text`, push `e.prompt.files`, set `e.metadata`, `e.delivery`. Edits become the canonical persisted user input; no typed rejection API; hooks must be retry-safe; not run for synthetic/shell messages. https://opencode.ai/docs/build/plugins/server
  - `ctx.session.hook("context", e => e.system.push({type:"text", text}))`: runs before every agent-loop model call incl. tool continuations; affects only the outgoing call, not persisted history; also `e.messages`, `e.tools`, `e.options`. Separate registration needed for `compaction`, `generate`, `title`. [V]
- Shell/exec: V2 `ctx.shell.hook("create.before")` only modifies agent shell commands; a plugin runs its own subprocess with its own process API (`node:child_process`/`Bun.spawn`; migration table says the V1 `$` Bun helper has no V2 equivalent). [V/I]
```ts
import { Plugin } from "@opencode/plugin";
import { spawnSync } from "node:child_process";
export default Plugin.define({
  id: "cora",
  setup(ctx) {
    ctx.session.hook("prompt", (e) => {
      const r = spawnSync("cora", ["hook", "context", "--prompt", e.prompt.text],
        { cwd: ctx.location.directory, encoding: "utf8", timeout: 15000 });
      if (r.status === 0 && r.stdout.trim()) e.prompt.text += "\n\n" + r.stdout.trim();
    });
  },
});
```
  (hook callbacks may be async per `Promise<void> | void` signatures shown for tool/shell hooks; for `session.hook` the doc example is sync [I].)
- V1 equivalents: `chat.message`, `experimental.chat.system.transform` (`output.system.push(str)`), `tool.execute.before/after`, `shell.env`, `event`; export `server()` returning hook object; dual export supported (`{...Plugin.define(...), async server(){...}}`, V1 object form needs opencode >= 1.18.29). https://opencode.ai/docs/build/plugins/migrate-v1
- Timeouts/failure behavior of plugin hooks: [unverified]. Plugin errors at load: [unverified].
- Plugin can add persistent per-session instruction: `ctx.session.instructions.entry.put({sessionID,key,value})` (experimental, announced at next step boundary without a message turn). [V]
- Install CLI: `opencode plugin add <pkg>`, `plugin list|check|update|remove`; reloads on file change under watched dirs, else `opencode service restart`.

### (d) Instruction files
- `AGENTS.md` project (walks up from cwd), global `~/.config/opencode/AGENTS.md`; fallbacks `CLAUDE.md`, `~/.claude/CLAUDE.md` used ONLY if no AGENTS.md exists in that category (first match wins). Extra: `instructions: ["file", "glob", "https://..."]` in config (all combined with AGENTS.md; remote fetch 5 s timeout). `/init` generates AGENTS.md. https://opencode.ai/docs/rules , https://opencode.ai/docs/config
- Managed block: allowed in practice (plain Markdown); a safer alternative is `instructions: [".cora/instructions.md"]` leaving AGENTS.md untouched [I].

### (e) Surprises
- V2 keys: `plugins`, `permissions` (array of `{action,resource,effect}`), `mcp.servers`, `skills`, `commands`, `agents.<id>.system`. V1: `plugin`, `permission`, `mcp.<name>`. Version detect needed.
- **Permission defaults**: base policy allows everything (`*`/`*`/allow) except `.env` reads ask; unmatched = ask; `external_directory` default allow. MCP permission action = `<server>_<tool>`, resource `*`. So cora MCP tools are allowed by default for primary agents. Sub-agent `explore` denies everything except shell/read/glob/grep/web/external_directory, so **cora tools are denied for `explore`** unless a rule allows `cora_*`. `plan` denies edits. https://opencode.ai/docs/permissions
- `~/.claude/skills`/`CLAUDE.md` compatibility means a Claude install is seen by opencode too (later-wins vs omp/pi first-wins differences).
- Project `.opencode/config` overrides ALL direct `opencode.json` regardless of depth (merge order).

### Most reliable
`prompt` hook injection (V2) or `experimental.chat.system.transform` (V1) + AGENTS.md block + `codemode:false`. [I]

---------------------------------------------------------------------------------------------------

## Comparison table

| Agent | MCP config (global / project) | Skills dir (global / project) | Pre-prompt context injection | Instruction file (managed block ok?) |
|---|---|---|---|---|
| Claude Code | `~/.claude.json` (user/local) / `.mcp.json` (needs approval); `alwaysLoad` | `~/.claude/skills/<n>/SKILL.md` / `.claude/skills/<n>/SKILL.md` | `UserPromptSubmit` (+`SessionStart`) command hook, stdout/`additionalContext`, 30 s | `CLAUDE.md`, `~/.claude/CLAUDE.md` (yes, markers) |
| Hermes | `~/.hermes/config.yaml` `mcp_servers:` / none found | `~/.hermes/skills/<cat>/<n>/`, `skills.external_dirs` / `<git>/.hermes/skills`, `.agents/skills` (needs `hermes skills trust`) | `hooks.pre_llm_call` shell hook in config.yaml, stdout `{"context":...}` appended to user msg; consent allowlist | `.hermes.md` > `AGENTS.override.md` > `AGENTS.md` > `CLAUDE.md` (first match only; injection scanner) |
| omp | `~/.omp/agent/mcp.json` / `.omp/mcp.json` (+ imports `.claude`, `opencode.json`, `.mcp.json`) | `~/.omp/agent/skills` [I], `~/.agents/skills` / `.omp/skills` [I], `.claude/skills`, `.agents/skills` (user foreign opt-in) | TS extension `before_agent_start` / `input` via `pi.exec`; `.omp/extensions/`, `~/.omp/agent/extensions/` | `.omp/AGENTS.md`, `~/.omp/agent/AGENTS.md` (shadows others), `AGENTS.md`, `CLAUDE.md`; `RULES.md` sticky |
| pi | `~/.pi/agent/mcp.json` / `.pi/mcp.json` (trust), `exposure:"direct"` | `~/.pi/agent/skills`, `~/.agents/skills` / `.pi/skills`, `.agents/skills` | TS extension `before_agent_start` (message/system sections) or `input`; `~/.pi/agent/extensions/`, `.pi/extensions/` | `AGENTS.md`/`CLAUDE.md` (agent dir, cwd, parents), `APPEND_SYSTEM.md` |
| opencode | `~/.config/opencode/opencode.json(c)` / `./opencode.json(c)`, `.opencode/opencode.json(c)`; `mcp.servers` (V2), `codemode:false` | `~/.config/opencode/skills`, `~/.claude/skills`, `~/.agents/skills` / `.opencode/skills`, `.claude/skills`, `.agents/skills` | plugin `session.hook("prompt")` / `"context"` (V2); V1 `chat.message`, `experimental.chat.system.transform`; `.opencode/plugins/`, `~/.config/opencode/plugins/` | `AGENTS.md` (else `CLAUDE.md`), `~/.config/opencode/AGENTS.md`, `instructions: []` |

Name-collision notes for shared skill dirs: `~/.claude/skills` is read by Claude, opencode (compat), omp (opt-in only); `~/.agents/skills` by opencode, pi, omp, Hermes (via `external_dirs` only) - installing the same skill name with identical content in both is deduped by omp, first-wins in pi (warning), later-wins in opencode. Prefer agent-native dirs per agent, or one shared dir plus identical content.

## Facts NOT verified (say so, do not guess)
- Claude: exact JSON placement/exact semantic of `alwaysLoad` in a stdio entry (scraper stripped code blocks); CLAUDE.md memory page not read (import syntax, size limits).
- Hermes: exact MCP tool prefix (`mcp_<server>_<tool>` vs `mcp__<server>__<tool>` - two pages disagree); schema of `~/.hermes/shell-hooks-allowlist.json` (for pre-approving); `plugin.yaml` schema; whether a project-level MCP/config file exists; whether a stdout line that is not JSON (plain text) is injected for `pre_llm_call` (docs only show JSON `{"context":..}`).
- omp: exact MCP tool-name separator (`mcp__<server>_<tool>` vs `mcp_<server>_<tool>`; scraper dropped `<>` placeholders); whether MCP tools are listed to the model or deferred by default; exact skills directory names under `.omp`; exact return shape of `before_agent_start` in the extension runner (message vs system-prompt override fields); whether omp honors Claude `settings.json` hooks; omp version history of MCP.
- pi: which pi version introduced built-in MCP; whether `~/.claude/skills` is read; extension `before_agent_start` `message.display:false` acceptance; permission/security defaults (security.md not read).
- opencode: timeouts and error behavior of plugin hooks; whether `session.hook` callbacks may be async; V1 config key shapes (inferred from migration guide, not read directly); exact V2 release/version number that switched defaults; `opencode.json` `plugins` entries must be directories (doc says so; relative to config file).
- All: whether `cora` binary is on PATH inside each agent's spawned environment (Hermes stdio filters env; others inherit) - runtime test needed. Nothing here was executed.
