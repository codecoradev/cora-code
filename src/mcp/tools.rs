//! MCP tool handlers — implement the actual tool logic.
//!
//! Each handler takes JSON params and returns a ToolResult.

use std::sync::LazyLock;

use crate::engine::diff_parser;
use crate::engine::profiles;
use crate::engine::rules;
use crate::engine::secrets_scanner;
use crate::engine::security_scanner;

use super::protocol::{Tool, ToolResult};

/// Shared Tokio runtime for MCP tool handlers — created once, reused across calls.
static MCP_RUNTIME: LazyLock<tokio::runtime::Runtime> =
    LazyLock::new(|| tokio::runtime::Runtime::new().expect("Failed to create MCP tokio runtime"));

/// List all available MCP tools.
pub fn list_tools() -> Vec<Tool> {
    vec![
        // ─── Review & Security ───
        Tool {
            name: "cora.list_rules".to_string(),
            description: "List all active review rules, quality profiles, and security patterns for this project.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "cora.check_snippet".to_string(),
            description: "Check a code snippet against cora's deterministic rules (secrets, security patterns). No LLM call.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Code snippet to check" },
                    "language": { "type": "string", "description": "Language of the snippet (e.g., 'rs', 'py', 'go')" }
                },
                "required": ["code"]
            }),
        },
        // ─── Config ───
        Tool {
            name: "cora.get_quality_gate".to_string(),
            description: "Get the current quality gate configuration and thresholds.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "cora.get_config".to_string(),
            description: "Get the effective cora configuration for this project (without secrets).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "section": { "type": "string", "description": "Config section to get (e.g., 'quality_gate', 'provider', 'rules')" }
                },
                "required": []
            }),
        },
        Tool {
            name: "cora.list_profiles".to_string(),
            description: "List all available quality profiles (built-in and custom).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ─── Code Intelligence ───
        Tool {
            name: "cora.search_symbols".to_string(),
            description: "Search the symbol index for code intelligence. Returns matching symbols with file location, kind, and signature. Requires `cora index` to be run first.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query (symbol name or keyword)" },
                    "kind": { "type": "string", "description": "Filter by kind: function, struct, enum, trait, method, constant, module" },
                    "file": { "type": "string", "description": "Filter by file path prefix" },
                    "language": { "type": "string", "description": "Filter by language (rs, py, ts, go, etc.)" },
                    "limit": { "type": "integer", "description": "Max results (default 50)", "default": 50 }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "cora.find_callers".to_string(),
            description: "Find all callers of a symbol (who calls this function/method?). Uses reverse call graph traversal.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to find callers for" },
                    "limit": { "type": "integer", "description": "Max results (default 50)", "default": 50 }
                },
                "required": ["symbol"]
            }),
        },
        Tool {
            name: "cora.find_impact".to_string(),
            description: "Analyze the blast radius of changing a symbol. Returns all affected symbols up to the specified depth.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Symbol name to analyze" },
                    "depth": { "type": "integer", "description": "Traversal depth (default 3)", "default": 3 }
                },
                "required": ["symbol"]
            }),
        },
        Tool {
            name: "cora.find_affected_tests".to_string(),
            description: "Find test files affected by source code changes. Uses call graph + naming conventions.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Changed source files"
                    }
                },
                "required": ["files"]
            }),
        },
        Tool {
            name: "cora.index_status".to_string(),
            description: "Check if a symbol index exists and get statistics (total symbols, files, languages).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        // ─── Review Pipeline (Phase 2) ───
        Tool {
            name: "cora.review_diff".to_string(),
            description: "Review a git diff using cora's full pipeline (deterministic rules + LLM). Returns issues, quality gate status, and severity breakdown. Note: makes an LLM API call and requires API key.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "diff": { "type": "string", "description": "Git diff text to review" },
                    "min_severity": { "type": "string", "description": "Minimum severity to report: info, minor, major, critical (default: info)" }
                },
                "required": ["diff"]
            }),
        },
        Tool {
            name: "cora.get_debt".to_string(),
            description: "Get tech debt report from review history. Returns quality score, finding counts, severity breakdown, and trend.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "since": { "type": "string", "description": "Filter since date or git tag (e.g., 'v0.5.0')" },
                    "branch": { "type": "string", "description": "Filter by branch name" }
                },
                "required": []
            }),
        },
        // ─── Context Enrichment (Phase 3) ───
        Tool {
            name: "cora.get_project_info".to_string(),
            description: "Get project context: repository name, current branch, cora version, and whether a symbol index exists.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "cora.get_memory".to_string(),
            description: "Recall project memories from Uteke (if installed). Returns relevant memories from previous reviews and code patterns.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Recall query (e.g., 'auth patterns', 'review history')" }
                },
                "required": ["query"]
            }),
        },
        // Brain Mode (Phase 3)
        Tool {
            name: "cora.brain_search".to_string(),
            description: "Hybrid code search: FTS5 + vector embeddings + graph proximity → RRF fusion. Better than plain text search for semantic code queries.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query (code concept, symbol name, or description)" },
                    "limit": { "type": "integer", "description": "Max results (default: 20)" }
                },
                "required": ["query"]
            }),
        },
        // ─── Agent Install ───
        Tool {
            name: "cora.install".to_string(),
            description: "Detect installed AI coding agents and configure Cora as an MCP server. List detected agents or install configuration.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "list": { "type": "boolean", "description": "List detected agents without installing" },
                    "agents": { "type": "string", "description": "Specific agents to install (comma-separated)" },
                    "dry_run": { "type": "boolean", "description": "Show what would be changed without writing" }
                },
                "required": []
            }),
        },
        // ─── Dead Code Detection ───
        Tool {
            name: "cora.dead_code".to_string(),
            description: "Find potentially dead code — functions/methods with no callers in the codebase.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "include_tests": { "type": "boolean", "description": "Include test functions in results" },
                    "min_lines": { "type": "integer", "description": "Minimum lines of code to report" }
                },
                "required": []
            }),
        },
        // ─── Graph Query DSL ───
        Tool {
            name: "cora.query".to_string(),
            description: "Query the code graph with simple patterns (e.g. 'main -> *', '* -> authenticate', 'MyStruct'). Returns matching symbols and edges.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Graph query pattern (e.g. 'main -> *', '* -> main')" },
                    "limit": { "type": "integer", "description": "Max results (default: 50)" }
                },
                "required": ["query"]
            }),
        },
    ]
}

/// Dispatch a tool call to the appropriate handler.
pub fn handle_tool_call(name: &str, params: &serde_json::Value) -> ToolResult {
    match name {
        // Review & Security
        "cora.list_rules" => handle_list_rules(),
        "cora.check_snippet" => handle_check_snippet(params),
        // Config
        "cora.get_quality_gate" => handle_get_quality_gate(),
        "cora.get_config" => handle_get_config(params),
        "cora.list_profiles" => handle_list_profiles(),
        // Code Intelligence
        "cora.search_symbols" => handle_search_symbols(params),
        "cora.find_callers" => handle_find_callers(params),
        "cora.find_impact" => handle_find_impact(params),
        "cora.find_affected_tests" => handle_find_affected_tests(params),
        "cora.index_status" => handle_index_status(),
        // Review Pipeline (Phase 2)
        "cora.review_diff" => handle_review_diff(params),
        "cora.get_debt" => handle_get_debt(params),
        // Context Enrichment (Phase 3)
        "cora.get_project_info" => handle_get_project_info(),
        "cora.get_memory" => handle_get_memory(params),
        // Brain Mode (Phase 3)
        "cora.brain_search" => handle_brain_search(params),
        // Agent Install
        "cora.install" => handle_install(params),
        // Dead Code Detection
        "cora.dead_code" => handle_dead_code(params),
        // Graph Query DSL
        "cora.query" => handle_query(params),
        _ => ToolResult::error(format!("Unknown tool: {name}")),
    }
}

fn handle_list_rules() -> ToolResult {
    let mut sections = Vec::new();

    // Built-in rule engine rules
    sections.push("## Rule Engine".to_string());
    let builtin_rules = rules::builtin::builtin_rules();
    for rule in &builtin_rules {
        sections.push(format!(
            "- **{}**: {} (severity: {:?})",
            rule.id, rule.message, rule.severity
        ));
    }

    // Secret patterns
    sections.push("\n## Secret Patterns".to_string());
    sections.push(format!(
        "{} built-in secret detection patterns (AWS, GitHub, OpenAI, Anthropic, etc.)",
        "12"
    ));

    // Security patterns
    sections.push("\n## Security Patterns".to_string());
    sections.push("11 static security patterns:".to_string());
    for pattern in security_scanner::PATTERNS {
        sections.push(format!(
            "- **{}**: {} ({:?})",
            pattern.id, pattern.name, pattern.severity
        ));
    }

    ToolResult::text(sections.join("\n"))
}

fn handle_check_snippet(params: &serde_json::Value) -> ToolResult {
    let code = match params.get("code").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ToolResult::error("Missing required parameter: code"),
    };

    let lang = params
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Create a fake diff chunk from the snippet
    let fake_diff = format!(
        "diff --git a/snippet.{lang} b/snippet.{lang}\n--- a/snippet.{lang}\n+++ b/snippet.{lang}\n@@ -0,0 +1,{count} @@\n{added}",
        count = code.lines().count(),
        added = code
            .lines()
            .map(|l| format!("+{l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let chunks = diff_parser::parse_diff(&fake_diff);

    let mut findings = Vec::new();

    // Run secrets scanner
    let secrets = secrets_scanner::scan_secrets(&chunks, 10);
    findings.extend(secrets);

    // Run security scanner
    let security = security_scanner::scan_security(&chunks, 10);
    findings.extend(security);

    if findings.is_empty() {
        ToolResult::text("✅ No issues found in snippet by deterministic scanners.")
    } else {
        let mut lines = vec![format!("Found {} issue(s):\n", findings.len())];
        for f in &findings {
            lines.push(format!(
                "- **{}** ({}): {} — {}",
                f.rule_id,
                f.severity.label(),
                f.title,
                f.body
            ));
        }
        ToolResult::text(lines.join("\n"))
    }
}

fn handle_get_quality_gate() -> ToolResult {
    match load_project_config() {
        Ok(config) => {
            let qg = &config.quality_gate;
            let output = format!(
                "## Quality Gate\n- **Enabled**: {}\n- **Thresholds**:\n  - max_critical: {}\n  - max_major: {}\n  - max_minor: {}\n  - max_security: {}\n- **Categories**: {}",
                qg.enabled,
                qg.thresholds.max_critical,
                qg.thresholds.max_major,
                qg.thresholds.max_minor,
                qg.thresholds.max_security,
                qg.categories.len(),
            );
            ToolResult::text(output)
        }
        Err(e) => ToolResult::error(format!("Failed to load config: {e}")),
    }
}

fn handle_get_config(params: &serde_json::Value) -> ToolResult {
    let section = params.get("section").and_then(|v| v.as_str());

    match load_project_config() {
        Ok(config) => {
            let output = match section {
                Some("provider") => {
                    format!(
                        "Provider: {} ({})",
                        config.provider.provider, config.provider.model
                    )
                }
                Some("quality_gate") => format!(
                    "Enabled: {}, max_critical: {}, max_security: {}",
                    config.quality_gate.enabled,
                    config.quality_gate.thresholds.max_critical,
                    config.quality_gate.thresholds.max_security,
                ),
                Some("rules") => format!("{} custom rules configured", config.rules.len()),
                Some("focus") => format!("Focus areas: {}", config.focus.join(", ")),
                _ => {
                    // Return safe overview (no secrets)
                    serde_json::to_string_pretty(&serde_json::json!({
                        "provider": config.provider.provider,
                        "model": config.provider.model,
                        "focus": config.focus,
                        "quality_gate_enabled": config.quality_gate.enabled,
                        "rules_count": config.rules.len(),
                    }))
                    .unwrap_or_else(|_| "Failed to serialize config".to_string())
                }
            };
            ToolResult::text(output)
        }
        Err(e) => ToolResult::error(format!("Failed to load config: {e}")),
    }
}

fn handle_list_profiles() -> ToolResult {
    let mut lines = vec!["## Available Quality Profiles\n".to_string()];

    for name in profiles::BUILTIN_PROFILES {
        if let Some(p) = profiles::load_builtin(name) {
            lines.push(format!(
                "### {}\n{}\nFocus areas: {}\n",
                p.name,
                p.description,
                p.focus_areas
                    .iter()
                    .map(|a| a.id.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
    }

    ToolResult::text(lines.join("\n"))
}

// ─── Code Intelligence Handlers ───

/// Open the global index database and resolve project_id for the project root.
/// Uses project root detection (walks up from CWD looking for markers).
/// Returns helpful error if not found.
fn open_index_db() -> anyhow::Result<(rusqlite::Connection, i64)> {
    let db_path = crate::data_dir::graph_db_path();
    if !db_path.exists() {
        anyhow::bail!("No symbol index found. Run 'cora index' first to build the index.");
    }
    let conn = crate::index::open_global_index()?;
    let (project_id, _root) = crate::index::resolve_project_id(&conn)?;
    Ok((conn, project_id))
}

fn handle_search_symbols(params: &serde_json::Value) -> ToolResult {
    let query_text = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolResult::error("Missing required parameter: query"),
    };

    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .map(crate::index::SymbolKind::from_str);
    let file_prefix = params
        .get("file")
        .and_then(|v| v.as_str())
        .map(String::from);
    let language = params
        .get("language")
        .and_then(|v| v.as_str())
        .map(String::from);
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    let query = crate::index::SymbolQuery {
        text: Some(query_text.to_string()),
        kind,
        file_prefix,
        language,
        limit,
    };

    match crate::index::search(&conn, project_id, &query) {
        Ok(results) => {
            if results.is_empty() {
                return ToolResult::text(format!("No symbols found matching '{query_text}'."));
            }
            let json: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.symbol.name,
                        "kind": r.symbol.kind.as_str(),
                        "file": r.symbol.file,
                        "line": r.symbol.line,
                        "signature": r.symbol.signature,
                        "language": r.symbol.language,
                        "score": r.score,
                    })
                })
                .collect();
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Search failed: {e}")),
    }
}

fn handle_find_callers(params: &serde_json::Value) -> ToolResult {
    let symbol = match params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("Missing required parameter: symbol"),
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    match crate::index::graph::find_callers(&conn, project_id, symbol, limit) {
        Ok(callers) => {
            if callers.is_empty() {
                return ToolResult::text(format!("No callers found for '{symbol}'."));
            }
            let json: Vec<serde_json::Value> = callers
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "caller": c.caller,
                        "file": c.file,
                        "line": c.line,
                    })
                })
                .collect();
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Find callers failed: {e}")),
    }
}

fn handle_find_impact(params: &serde_json::Value) -> ToolResult {
    let symbol = match params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ToolResult::error("Missing required parameter: symbol"),
    };
    let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u32;

    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    match crate::index::graph::impact_analysis(&conn, project_id, symbol, depth) {
        Ok(impact) => {
            if impact.is_empty() {
                return ToolResult::text(format!("No impact found for '{symbol}'."));
            }
            let json: Vec<serde_json::Value> = impact
                .iter()
                .map(|n| {
                    serde_json::json!({
                        "symbol": n.symbol,
                        "file": n.file,
                        "line": n.line,
                        "depth": n.depth,
                    })
                })
                .collect();
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Impact analysis failed: {e}")),
    }
}

fn handle_find_affected_tests(params: &serde_json::Value) -> ToolResult {
    let files: Vec<String> = match params.get("files").and_then(|v| v.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        None => {
            return ToolResult::error("Missing required parameter: files (array of file paths)");
        }
    };
    if files.is_empty() {
        return ToolResult::error("Parameter 'files' must not be empty");
    }

    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    let patterns = ["test", "spec", "_test", "_spec"];
    let mut affected: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Batch fetch all symbols for all files in a single query
    let all_symbols: Vec<String> = {
        let placeholders = files.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let n = files.len() + 1;
        let sql = format!(
            "SELECT DISTINCT name FROM symbols WHERE file IN ({placeholders}) AND project_id = ?{n}"
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("DB error: {e}")),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = files
            .iter()
            .map(|f| Box::new(f.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        params.push(Box::new(project_id));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = match stmt.query_map(param_refs.as_slice(), |row| row.get::<_, String>(0)) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("DB error: {e}")),
        };
        rows.filter_map(|r| r.ok()).collect()
    };

    // Deduplicate and traverse call graph once
    {
        let mut seen_syms: std::collections::HashSet<String> = std::collections::HashSet::new();
        for sym_name in &all_symbols {
            if seen_syms.insert(sym_name.clone()) {
                if let Ok(callers) =
                    crate::index::graph::find_callers(&conn, project_id, sym_name, 100)
                {
                    for caller in callers {
                        if patterns.iter().any(|p| caller.file.contains(*p)) {
                            affected.insert(caller.file.clone());
                        }
                    }
                }
            }
        }
    }

    // Strategy 2: naming convention — batch all test name candidates
    let mut test_names: Vec<String> = Vec::new();
    for file in &files {
        let stem = file
            .rsplit('/')
            .next()
            .unwrap_or(file)
            .rsplit('.')
            .next()
            .unwrap_or("");
        test_names.extend_from_slice(&[
            format!("{stem}_test.rs"),
            format!("tests/{stem}.rs"),
            format!("{stem}_test.go"),
            format!("test_{stem}.py"),
            format!("{stem}.test.ts"),
            format!("{stem}.spec.ts"),
        ]);
    }

    // Query with a single LIKE batch, scoped to project
    {
        let n = test_names.len() + 1;
        let sql = format!(
            "SELECT DISTINCT path FROM files WHERE (path LIKE '%' || ?1 OR {}) AND project_id = ?{n}",
            test_names
                .iter()
                .enumerate()
                .skip(1)
                .map(|(i, _)| format!("path LIKE '%' || ?{}", i + 1))
                .collect::<Vec<_>>()
                .join(" OR ")
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("DB error: {e}")),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = test_names
            .iter()
            .map(|t| Box::new(t.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        params.push(Box::new(project_id));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        if let Ok(rows) = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, String>(0)) {
            for row in rows.map_while(Result::ok) {
                affected.insert(row);
            }
        }
    }

    let mut sorted: Vec<String> = affected.into_iter().collect();
    sorted.sort();

    let json = serde_json::json!({
        "affected_tests": sorted,
        "count": sorted.len(),
    });
    ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
}

fn handle_index_status() -> ToolResult {
    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    match crate::index::index_stats(&conn, project_id) {
        Ok(stats) => {
            let json = serde_json::json!({
                "exists": true,
                "total_symbols": stats.total_symbols,
                "total_files": stats.total_files,
                "db_size_bytes": stats.db_size_bytes,
                "symbols_by_kind": stats.symbols_by_kind,
                "symbols_by_language": stats.symbols_by_language,
            });
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Failed to get stats: {e}")),
    }
}

// ─── Review Pipeline Handlers (Phase 2) ───

fn handle_review_diff(params: &serde_json::Value) -> ToolResult {
    let diff = match params.get("diff").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return ToolResult::error("Missing required parameter: diff"),
    };
    if diff.trim().is_empty() {
        return ToolResult::error("Diff is empty");
    }

    // Load config + build LLM config
    let config = match load_project_config() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to load config: {e}")),
    };

    let llm_config = match crate::config::loader::build_llm_config(&config, None) {
        Ok(c) => c,
        Err(e) => {
            return ToolResult::error(format!(
                "Failed to build LLM config: {e}. Is API key set? Use 'cora auth login'."
            ));
        }
    };

    // Run review synchronously using the shared runtime
    let result = MCP_RUNTIME.block_on(crate::engine::review::review_diff_with_cache(
        &config,
        &llm_config,
        diff,
        false, // no stream
        true,  // use cache
        true,  // quiet
        None,  // no memory
    ));

    match result {
        Ok(response) => {
            let json = serde_json::json!({
                "summary": response.summary,
                "total_issues": response.issues.len(),
                "should_block": response.should_block,
                "issues": response.issues.iter().map(|i| serde_json::json!({
                    "title": i.title,
                    "severity": i.severity.label(),
                    "file": i.file,
                    "line": i.line,
                    "type": i.issue_type,
                    "body": i.body,
                })).collect::<Vec<_>>(),
                "gate": if config.quality_gate.enabled {
                    let gate = crate::engine::quality_gate::evaluate(&response.issues, &config.quality_gate);
                    serde_json::json!({
                        "status": format!("{:?}", gate.status),
                        "total_findings": gate.total_findings,
                    })
                } else {
                    serde_json::json!({"enabled": false})
                },
            });
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Review failed: {e}")),
    }
}

fn handle_get_debt(params: &serde_json::Value) -> ToolResult {
    let _since = params.get("since").and_then(|v| v.as_str());
    let _branch = params.get("branch").and_then(|v| v.as_str());

    let config = load_project_config().unwrap_or_default();

    let snapshots = crate::engine::debt_tracker::load_snapshots(config.debt.history_dir.as_deref());

    if snapshots.is_empty() {
        return ToolResult::text(
            "No debt snapshots found. Run 'cora review' or 'cora commit' to generate history.",
        );
    }

    let report = crate::engine::debt_tracker::aggregate(&snapshots);

    let json = serde_json::json!({
        "quality_score": report.quality_score_avg,
        "quality_score_change": report.quality_score_change,
        "trend": report.trend,
        "total_reviews": report.reviews_analyzed,
        "total_findings": report.total_findings,
        "change_from_previous": report.change_from_previous,
        "findings": report.findings,
        "categories": report.categories.iter().map(|c| serde_json::json!({
            "name": c.name,
            "count": c.count,
            "change": c.change,
            "trend": c.trend,
        })).collect::<Vec<_>>(),
        "period_start": report.period_start.map(|t| t.to_rfc3339()),
        "period_end": report.period_end.map(|t| t.to_rfc3339()),
    });
    ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
}

// ─── Context Enrichment Handlers (Phase 3) ───

fn handle_get_project_info() -> ToolResult {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to get cwd: {e}")),
    };

    let repo_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    let index_exists = crate::data_dir::graph_db_path().exists();

    let json = serde_json::json!({
        "repository": repo_name,
        "branch": branch,
        "cora_version": env!("CARGO_PKG_VERSION"),
        "index_exists": index_exists,
        "working_dir": cwd.to_string_lossy(),
    });
    ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
}

fn handle_get_memory(params: &serde_json::Value) -> ToolResult {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolResult::error("Missing required parameter: query"),
    };

    // Check if uteke is available
    if which::which("uteke").is_err() {
        return ToolResult::text(
            "Uteke is not installed. Memory features require 'uteke' CLI. Install from https://github.com/codecoradev/uteke",
        );
    }

    // Determine project name from git
    let project = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout).ok().and_then(|s| {
                s.trim()
                    .rsplit('/')
                    .next()
                    .map(|s| s.trim_end_matches(".git").to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string());

    let mut backend = crate::engine::memory::MemoryBackend::default();
    backend.detect();

    if !backend.is_available() {
        return ToolResult::text(
            "Uteke detected but not accessible. Run 'uteke doctor' to diagnose.",
        );
    }

    let memories = backend.recall_context(&project);

    if memories.is_empty() {
        return ToolResult::text(format!(
            "No memories found for '{query}' in namespace 'cora'."
        ));
    }

    // Filter memories by query relevance (simple contains check)
    let query_lower = query.to_lowercase();
    let filtered: Vec<&String> = memories
        .iter()
        .filter(|m| m.to_lowercase().contains(&query_lower))
        .collect();

    let results = if filtered.is_empty() {
        memories.iter().take(5).collect()
    } else {
        filtered
    };

    let json: Vec<serde_json::Value> = results
        .iter()
        .enumerate()
        .map(|(i, m)| {
            serde_json::json!({
                "index": i + 1,
                "content": m,
            })
        })
        .collect();

    ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
}

// ─── Brain Mode Handlers (Phase 3) ───

fn handle_brain_search(params: &serde_json::Value) -> ToolResult {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolResult::error("Missing required parameter: query"),
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    match crate::index::brain::brain_search(&conn, project_id, query, limit) {
        Ok(results) => {
            if results.is_empty() {
                return ToolResult::text(format!("No results found for '{query}'."));
            }
            let json: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.name,
                        "kind": r.kind,
                        "file": r.file,
                        "line": r.line,
                        "signature": r.signature,
                        "score": r.score,
                        "signals": r.signals,
                    })
                })
                .collect();
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Brain search failed: {e}")),
    }
}

// ─── Agent Install Handler ───

fn handle_install(params: &serde_json::Value) -> ToolResult {
    let list = params
        .get("list")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let agents = params
        .get("agents")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let dry_run = params
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let opts = crate::commands::install::InstallOptions {
        list,
        agents,
        dry_run,
        force: false,
        yes: true, // MCP is non-interactive
    };

    match crate::commands::install::execute_install(&opts) {
        Ok(msg) => ToolResult::text(msg),
        Err(e) => ToolResult::error(format!("Install failed: {e:#}")),
    }
}

// ─── Dead Code Detection Handler ───

fn handle_dead_code(params: &serde_json::Value) -> ToolResult {
    let (conn, project_id) = match open_index_db() {
        Ok((c, pid)) => (c, pid),
        Err(e) => return ToolResult::error(e.to_string()),
    };

    let include_tests = params
        .get("include_tests")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let min_lines = params
        .get("min_lines")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let opts = crate::index::graph::DeadCodeOptions {
        include_tests,
        min_lines,
        entry_point_patterns: vec![],
    };

    match crate::index::graph::find_dead_code(&conn, project_id, &opts) {
        Ok(results) => {
            if results.is_empty() {
                return ToolResult::text("No dead code found.");
            }
            let json: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "name": r.name,
                        "kind": r.kind,
                        "file": r.file,
                        "line": r.line,
                        "reason": r.reason,
                    })
                })
                .collect();
            ToolResult::text(serde_json::to_string_pretty(&json).unwrap_or_default())
        }
        Err(e) => ToolResult::error(format!("Dead code detection failed: {e}")),
    }
}

// ─── Graph Query DSL Handler ───

fn handle_query(params: &serde_json::Value) -> ToolResult {
    let query = match params.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ToolResult::error("Missing required parameter: query"),
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    match crate::commands::query::execute_query_cli(query, true, limit) {
        Ok(output) => ToolResult::text(output),
        Err(e) => ToolResult::error(format!("Query failed: {e:#}")),
    }
}

/// Load project config safely (no API keys exposed).
fn load_project_config() -> anyhow::Result<crate::config::schema::Config> {
    let mut config = crate::config::schema::Config::default();
    let cwd = std::env::current_dir()?;

    if let Some((_, cora)) = crate::config::loader::find_cora_file(&cwd)? {
        cora.merge_into(&mut config)?;
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_tools_returns_tools() {
        let tools = list_tools();
        assert!(tools.len() >= 5);
        assert!(tools.iter().any(|t| t.name == "cora.list_rules"));
        assert!(tools.iter().any(|t| t.name == "cora.check_snippet"));
    }

    #[test]
    fn handle_unknown_tool() {
        let result = handle_tool_call("cora.unknown", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_list_rules() {
        let result = handle_tool_call("cora.list_rules", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content[0].text.contains("Rule Engine"));
        assert!(result.content[0].text.contains("Security Patterns"));
    }

    #[test]
    fn handle_check_snippet_clean() {
        let result = handle_tool_call(
            "cora.check_snippet",
            &serde_json::json!({"code": "let x = 1;", "language": "rs"}),
        );
        assert!(!result.is_error);
        assert!(result.content[0].text.contains("No issues"));
    }

    #[test]
    fn handle_check_snippet_secret() {
        let result = handle_tool_call(
            "cora.check_snippet",
            &serde_json::json!({"code": "key = 'AKIAIOSFODNN7EXAMPLE'", "language": "py"}),
        );
        assert!(!result.is_error);
        assert!(result.content[0].text.contains("issue"));
    }

    #[test]
    fn handle_check_snippet_missing_code() {
        let result = handle_tool_call("cora.check_snippet", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_list_profiles() {
        let result = handle_tool_call("cora.list_profiles", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content[0].text.contains("security-first"));
        assert!(result.content[0].text.contains("rust-strict"));
    }

    // ─── Code Intelligence Tool Tests ───

    #[test]
    fn list_tools_includes_code_intel() {
        let tools = list_tools();
        assert!(tools.iter().any(|t| t.name == "cora.search_symbols"));
        assert!(tools.iter().any(|t| t.name == "cora.find_callers"));
        assert!(tools.iter().any(|t| t.name == "cora.find_impact"));
        assert!(tools.iter().any(|t| t.name == "cora.find_affected_tests"));
        assert!(tools.iter().any(|t| t.name == "cora.index_status"));
    }

    #[test]
    fn handle_index_status_no_index() {
        // From test dir, there should be no index
        let result = handle_tool_call("cora.index_status", &serde_json::json!({}));
        // May error if no index — that's acceptable
        assert!(result.is_error || result.content[0].text.contains("total_symbols"));
    }

    #[test]
    fn handle_search_symbols_missing_query() {
        let result = handle_tool_call("cora.search_symbols", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_find_callers_missing_symbol() {
        let result = handle_tool_call("cora.find_callers", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_find_impact_missing_symbol() {
        let result = handle_tool_call("cora.find_impact", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_find_affected_tests_missing_files() {
        let result = handle_tool_call("cora.find_affected_tests", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_find_affected_tests_empty_files() {
        let result = handle_tool_call(
            "cora.find_affected_tests",
            &serde_json::json!({"files": []}),
        );
        assert!(result.is_error);
    }

    // ─── Phase 2 Tool Tests ───

    #[test]
    fn list_tools_includes_phase2() {
        let tools = list_tools();
        assert!(tools.iter().any(|t| t.name == "cora.review_diff"));
        assert!(tools.iter().any(|t| t.name == "cora.get_debt"));
    }

    #[test]
    fn handle_review_diff_missing_diff() {
        let result = handle_tool_call("cora.review_diff", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_review_diff_empty_diff() {
        let result = handle_tool_call("cora.review_diff", &serde_json::json!({"diff": ""}));
        assert!(result.is_error);
    }

    #[test]
    fn handle_get_debt_returns_data_or_error() {
        let result = handle_tool_call("cora.get_debt", &serde_json::json!({}));
        // Should either return data or error (no snapshots)
        // Accept any non-panic result — debt state depends on environment
        let _ = result;
    }

    // ─── Phase 3 Tool Tests ───

    #[test]
    fn list_tools_includes_phase3() {
        let tools = list_tools();
        assert!(tools.iter().any(|t| t.name == "cora.get_project_info"));
        assert!(tools.iter().any(|t| t.name == "cora.get_memory"));
    }

    #[test]
    fn handle_get_project_info() {
        let result = handle_tool_call("cora.get_project_info", &serde_json::json!({}));
        assert!(!result.is_error);
        assert!(result.content[0].text.contains("repository"));
        assert!(result.content[0].text.contains("cora_version"));
    }

    #[test]
    fn handle_get_memory_missing_query() {
        let result = handle_tool_call("cora.get_memory", &serde_json::json!({}));
        assert!(result.is_error);
    }

    #[test]
    fn total_tool_count() {
        let tools = list_tools();
        // Phase 1 (5) + code intel (5) + Phase 2 (2) + Phase 3 (3) + install (1) + dead_code (1) + query (1) = 18
        assert_eq!(tools.len(), 18);
    }
}
