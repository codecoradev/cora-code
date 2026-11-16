{{CORA_SKILL_MARKER}}

# cora code intelligence: commands and tools

Each command has an MCP tool with the same job; some agents show the tool names with underscores (`cora_find_callers`), others with a dot (`cora.find_callers`).

| Need | CLI | MCP tool |
|---|---|---|
| A question in words | `cora brain "<question>"` (`--limit`, `--json`) | `cora_brain_search` |
| A symbol by name | `cora explore <name>` (`--kind`, `--file`, `--language`, `--limit`, `--json`, `--no-semantic`) | `cora_search_symbols` |
| Who calls a function | `cora callers <name>` (`--limit`, `--strict`, `--json`; `Type::name` for one type) | `cora_find_callers` |
| What breaks if it changes | `cora impact <name>` (`--depth`, `--json`) | `cora_find_impact` |
| Tests to run for changed files | `cora affected` (`--stdin` reads the changed file paths, one per line, from standard input; `--depth`, `--limit`) | `cora_find_affected_tests` |
| A call chain | `cora trace <name>` (`--direction incoming` or `outgoing`, `--depth`, `--strict`) | |
| Unused functions | `cora dead-code` (`--include-tests`, `--include-public`, `--min-lines`) | `cora_dead_code` |
| Structured questions about the graph | `cora query` (`--limit`, `--include-std`) | `cora_query` |
| HTTP routes | `cora routes` (`--method`, `--prefix`) | |
| Module overview | `cora arch` (`--depth`) | |

## Reading the results

- A result with the signals `fts`, `vector` and `graph` was found by keyword, by meaning and by call-graph proximity; one found only by `vector` is a guess by meaning, check it.
- `--strict` leaves out calls whose receiver type is unknown: fewer results, fewer false positives.
- `dead-code` is a lead, not a verdict: dynamic dispatch, reflection and public API are not seen as calls.
- All commands take `--json` for machine-readable output.

## When the index looks wrong

- It is built from the project files and brought up to date by `cora index`, and automatically before a review or scan. After you change what is ignored (see "First time in a project" in SKILL.md), the next index run rebuilds it.
- `cora index --rebuild` drops it and starts over.
