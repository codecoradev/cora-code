//! `cora install` subcommand — auto-detect and configure AI coding agents for Cora MCP.
//!
//! Detects installed AI coding agents by checking for known config files/directories,
//! then writes MCP server config pointing to `cora mcp` for each detected agent.

use super::agent_config::{
    ConfigFormat, json_add_cora, json_has_cora, read_json_config, write_json_config,
};
use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

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

/// Install the cora MCP server entry into a JSON/JSONC agent config.
fn install_json_agent(path: &std::path::Path, force: bool, dry_run: bool) -> Result<String> {
    let mut config = read_json_config(path)?;

    if json_has_cora(&config) && !force {
        return Ok(format!(
            "  {} {} — cora entry already exists (use --force to overwrite)",
            "⏭ ".dimmed(),
            path.display()
        ));
    }

    json_add_cora(&mut config)?;

    if dry_run {
        Ok(format!(
            "  {} {} — would write cora MCP server entry",
            "🔍 ".cyan(),
            path.display()
        ))
    } else {
        write_json_config(path, &config)?;
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
            install_json_agent(&agent.config_path, opts.force, opts.dry_run)
        }
        ConfigFormat::Yaml => {
            // YAML agents are rare; delegate to agent_config module.
            use super::agent_config;
            let mut config = agent_config::read_yaml_config(&agent.config_path)?;

            if agent_config::yaml_has_cora(&config) && !opts.force {
                return Ok(format!(
                    "  {} {} — cora entry already exists (use --force to overwrite)",
                    "⏭ ".dimmed(),
                    agent.config_path.display()
                ));
            }

            agent_config::yaml_add_cora(&mut config)?;

            if opts.dry_run {
                Ok(format!(
                    "  {} {} — would write cora MCP server entry",
                    "🔍 ".cyan(),
                    agent.config_path.display()
                ))
            } else {
                agent_config::write_yaml_config(&agent.config_path, &config)?;
                Ok(format!(
                    "  {} {} — cora MCP server entry added",
                    "✓ ".green(),
                    agent.config_path.display()
                ))
            }
        }
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
