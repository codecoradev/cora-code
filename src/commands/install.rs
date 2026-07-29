//! `cora install` subcommand — auto-detect and configure AI coding agents for Cora MCP.//!
//! Detects installed AI coding agents by checking for known config files/directories,
//! then writes MCP server config pointing to `cora mcp` for each detected agent.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::PathBuf;

/// Config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFormat {
    Json,
    Jsonc,
    #[allow(dead_code)]
    Yaml,
}

/// Information about a detected AI coding agent.
#[derive(Debug, Clone)]
struct AgentInfo {
    name: &'static str,
    config_path: PathBuf,
    format: ConfigFormat,
}

/// Install subcommand options.
pub struct InstallOptions {
    /// List detected agents without installing.
    pub list: bool,
    /// Specific agents to install (comma-separated).
    pub agents: Option<String>,
    /// Dry run — show what would be changed.
    pub dry_run: bool,
    /// Overwrite existing cora entry.
    pub force: bool,
    /// Non-interactive mode.
    #[allow(dead_code)]
    pub yes: bool,
}

/// The Cora MCP server entry we inject into agent configs.
fn cora_mcp_entry() -> serde_json::Value {
    serde_json::json!({
        "command": "cora",
        "args": ["mcp"],
        "description": "Cora Code — AI code review, code intelligence, dead code detection"
    })
}

/// Build the list of known agents and their config paths.
fn known_agents(home: &std::path::Path) -> Vec<AgentInfo> {
    vec![
        AgentInfo {
            name: "cline",
            config_path: home.join(".cline/cline_mcp_settings.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "cursor",
            config_path: home.join(".cursor/mcp.json"),
            format: ConfigFormat::Jsonc,
        },
        AgentInfo {
            name: "windsurf",
            config_path: home.join(".codeium/windsurf/mcp_config.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "roo",
            config_path: home.join(".roo/mcp.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "copilot",
            config_path: home.join(".github-copilot/mcp.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "continue",
            config_path: home.join(".continue/config.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "amp",
            config_path: home.join(".amp/amprc.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "trae",
            config_path: home.join(".trae/mcp.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "kodu",
            config_path: home.join(".kodu/settings.json"),
            format: ConfigFormat::Json,
        },
        AgentInfo {
            name: "fabric",
            config_path: home.join(".fabric/mcp.json"),
            format: ConfigFormat::Json,
        },
    ]
}

/// Detect which agents are installed by checking if their config paths exist.
fn detect_agents() -> Result<Vec<AgentInfo>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let candidates = known_agents(&home);
    let detected: Vec<AgentInfo> = candidates
        .into_iter()
        .filter(|a| a.config_path.exists())
        .collect();
    Ok(detected)
}

/// Strip JSONC comments (// line comments and /* block comments */).
/// Simple regex approach — sufficient for config files.
fn strip_jsonc_comments(input: &str) -> String {
    let re = regex::Regex::new(r"//.*|/\*[\s\S]*?\*/").expect("valid regex");
    re.replace_all(input, "").to_string()
}

/// Merge the cora MCP server entry into a JSON/JSONC config file.
fn merge_json_config(path: &std::path::Path, force: bool, dry_run: bool) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    // For JSONC, strip comments before parsing
    let clean = strip_jsonc_comments(&raw);

    let mut config: serde_json::Value =
        serde_json::from_str(&clean).context("Failed to parse JSON config")?;

    let is_jsonc = raw.contains("//") || raw.contains("/*");
    let has_existing = config
        .get("mcpServers")
        .and_then(|m| m.get("cora"))
        .is_some();

    if has_existing && !force {
        return Ok(format!(
            "  {} {} — cora entry already exists (use --force to overwrite)",
            "⏭ ".dimmed(),
            path.display()
        ));
    }

    // Ensure mcpServers object exists
    let servers = config
        .as_object_mut()
        .context("Config root is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    servers
        .as_object_mut()
        .context("mcpServers is not an object")?
        .insert("cora".to_string(), cora_mcp_entry());

    if dry_run {
        Ok(format!(
            "  {} {} — would write cora MCP server entry",
            "🔍 ".cyan(),
            path.display()
        ))
    } else {
        let output = if is_jsonc {
            // Write back as JSONC with a header comment
            format!(
                "// Modified by cora install\n{}",
                serde_json::to_string_pretty(&config)?
            )
        } else {
            serde_json::to_string_pretty(&config)?
        };
        fs::write(path, &output).with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(format!(
            "  {} {} — cora MCP server entry added",
            "✓ ".green(),
            path.display()
        ))
    }
}

/// Merge the cora MCP server entry into a YAML config file.
fn merge_yaml_config(path: &std::path::Path, force: bool, dry_run: bool) -> Result<String> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;

    let mut config: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&raw).context("Failed to parse YAML config")?;

    let has_existing = config
        .get("mcpServers")
        .and_then(|m| m.get("cora"))
        .is_some();

    if has_existing && !force {
        return Ok(format!(
            "  {} {} — cora entry already exists (use --force to overwrite)",
            "⏭ ".dimmed(),
            path.display()
        ));
    }

    // Ensure mcpServers mapping exists
    let servers = config
        .as_mapping_mut()
        .context("Config root is not a YAML mapping")?
        .entry(serde_yaml_ng::Value::String("mcpServers".to_string()))
        .or_insert_with(|| serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()));

    if let Some(mapping) = servers.as_mapping_mut() {
        let cora_entry_yaml: serde_yaml_ng::Value =
            serde_yaml_ng::from_str("command: cora\nargs:\n  - mcp\ndescription: 'Cora Code — AI code review, code intelligence, dead code detection'")
                .expect("valid yaml");
        mapping.insert(
            serde_yaml_ng::Value::String("cora".to_string()),
            cora_entry_yaml,
        );
    }

    if dry_run {
        Ok(format!(
            "  {} {} — would write cora MCP server entry",
            "🔍 ".cyan(),
            path.display()
        ))
    } else {
        let output = serde_yaml_ng::to_string(&config)?;
        fs::write(path, output).with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(format!(
            "  {} {} — cora MCP server entry added",
            "✓ ".green(),
            path.display()
        ))
    }
}

/// Install the cora MCP server entry for a single agent.
fn install_agent(agent: &AgentInfo, opts: &InstallOptions) -> Result<String> {
    match agent.format {
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            merge_json_config(&agent.config_path, opts.force, opts.dry_run)
        }
        ConfigFormat::Yaml => merge_yaml_config(&agent.config_path, opts.force, opts.dry_run),
    }
}

/// Execute the `cora install` subcommand.
pub fn execute_install(opts: &InstallOptions) -> Result<String> {
    let all_agents = detect_agents()?;

    if all_agents.is_empty() {
        return Ok("No AI coding agents detected. Install an agent (e.g. Cline, Cursor, Windsurf) and run again.".to_string());
    }

    // Filter to specific agents if requested
    let agents: Vec<&AgentInfo> = if let Some(ref agent_list) = opts.agents {
        let requested: std::collections::HashSet<&str> =
            agent_list.split(',').map(|s| s.trim()).collect();
        all_agents
            .iter()
            .filter(|a| requested.contains(a.name))
            .collect()
    } else {
        all_agents.iter().collect()
    };

    if agents.is_empty() {
        if let Some(ref agent_list) = opts.agents {
            return Ok(format!(
                "None of the specified agents found: {agent_list}\nDetected agents: {}",
                all_agents
                    .iter()
                    .map(|a| a.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        unreachable!();
    }

    // --list mode
    if opts.list {
        let mut lines = vec![format!("Detected {} AI coding agent(s):\n", agents.len())];
        for agent in &agents {
            lines.push(format!(
                "  {} {}  {}",
                "● ".cyan(),
                agent.name.bold(),
                agent.config_path.display()
            ));
        }
        return Ok(lines.join("\n"));
    }

    // Install mode
    let mut lines = vec![format!(
        "Configuring cora MCP for {} agent(s)…{}",
        agents.len(),
        if opts.dry_run { " (dry run)" } else { "" }
    )];
    lines.push(String::new());

    for agent in &agents {
        let result = install_agent(agent, opts)?;
        lines.push(format!("{} {}", agent.name.bold(), result));
    }

    lines.push(String::new());
    lines.push("Done. Restart your AI agent to pick up the new MCP server.".to_string());

    Ok(lines.join("\n"))
}
