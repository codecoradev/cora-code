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
    /// List detected agents only.
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
    /// Remove cora MCP entry (uninstall mode).
    pub remove: bool,
    /// Validate agent configs after install/remove.
    pub validate: bool,
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

/// Names of all agents `cora install` knows how to configure.
pub fn known_agent_names() -> Vec<&'static str> {
    known_agents(std::path::Path::new(""))
        .into_iter()
        .map(|a| a.name)
        .collect()
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

/// Validate that a config file still parses correctly after modification.
fn validate_json_config(path: &std::path::Path) -> Result<()> {
    let config = read_json_config(path)?;
    // If it parses without error, it's valid.
    let _ = serde_json::to_string(&config)?;
    Ok(())
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

/// Remove the cora MCP server entry from a JSON/JSONC agent config.
fn uninstall_json_agent(path: &std::path::Path, dry_run: bool) -> Result<String> {
    use super::agent_config;

    let mut config = read_json_config(path)?;

    if !agent_config::json_has_cora(&config) {
        return Ok(format!(
            "  {} {} — no cora entry found",
            "⏭ ".dimmed(),
            path.display()
        ));
    }

    agent_config::json_remove_cora(&mut config);

    if dry_run {
        Ok(format!(
            "  {} {} — would remove cora MCP server entry",
            "🔍 ".cyan(),
            path.display()
        ))
    } else {
        write_json_config(path, &config)?;
        Ok(format!(
            "  {} {} — cora MCP server entry removed",
            "✓ ".green(),
            path.display()
        ))
    }
}

/// Install or remove the cora MCP server entry for a single agent.
fn install_agent(agent: &AgentInfo, opts: &InstallOptions) -> Result<String> {
    match agent.format {
        ConfigFormat::Json | ConfigFormat::Jsonc => {
            if opts.remove {
                uninstall_json_agent(&agent.config_path, opts.dry_run)
            } else {
                install_json_agent(&agent.config_path, opts.force, opts.dry_run)
            }
        }
        ConfigFormat::Yaml => {
            use super::agent_config;
            let mut config = agent_config::read_yaml_config(&agent.config_path)?;

            if opts.remove {
                if !agent_config::yaml_has_cora(&config) {
                    return Ok(format!(
                        "  {} {} — no cora entry found",
                        "⏭ ".dimmed(),
                        agent.config_path.display()
                    ));
                }
                agent_config::yaml_remove_cora(&mut config);
                if opts.dry_run {
                    Ok(format!(
                        "  {} {} — would remove cora MCP server entry",
                        "🔍 ".cyan(),
                        agent.config_path.display()
                    ))
                } else {
                    agent_config::write_yaml_config(&agent.config_path, &config)?;
                    Ok(format!(
                        "  {} {} — cora MCP server entry removed",
                        "✓ ".green(),
                        agent.config_path.display()
                    ))
                }
            } else {
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
}

/// The `/cora-review` skill for Claude Code (agent-as-reviewer, #647).
const CLAUDE_SKILL: &str = include_str!("cora_review_skill.md");

/// `~/.claude/skills/cora-review/SKILL.md`, when Claude Code is installed
/// (`~/.claude` exists).
fn claude_skill_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let claude = home.join(".claude");
    claude
        .is_dir()
        .then(|| claude.join("skills").join("cora-review").join("SKILL.md"))
}

/// Install or remove the `/cora-review` skill for Claude Code. `None` when
/// Claude Code is not installed.
fn install_claude_skill(opts: &InstallOptions) -> Option<Result<String>> {
    let path = claude_skill_path()?;
    Some(install_skill_at(&path, opts))
}

fn install_skill_at(path: &std::path::Path, opts: &InstallOptions) -> Result<String> {
    let name = "claude-code skill /cora-review";
    if opts.remove {
        if !path.exists() {
            return Ok(format!(
                "{} {} — no skill found",
                name.bold(),
                "⏭ ".dimmed()
            ));
        }
        if opts.dry_run {
            return Ok(format!(
                "{} {} {} — would remove",
                name.bold(),
                "🔍 ".cyan(),
                path.display()
            ));
        }
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir); // only if empty
        }
        return Ok(format!(
            "{} {} {} — removed",
            name.bold(),
            "✓ ".green(),
            path.display()
        ));
    }

    if path.exists() && !opts.force {
        let current = std::fs::read_to_string(path).unwrap_or_default();
        if current == CLAUDE_SKILL {
            return Ok(format!(
                "{} {} {} — up to date",
                name.bold(),
                "⏭ ".dimmed(),
                path.display()
            ));
        }
        return Ok(format!(
            "{} {} {} — exists and differs (use --force to overwrite)",
            name.bold(),
            "⏭ ".dimmed(),
            path.display()
        ));
    }
    if opts.dry_run {
        return Ok(format!(
            "{} {} {} — would write",
            name.bold(),
            "🔍 ".cyan(),
            path.display()
        ));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(path, CLAUDE_SKILL)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(format!(
        "{} {} {} — installed",
        name.bold(),
        "✓ ".green(),
        path.display()
    ))
}

/// Execute the `cora install` subcommand.
pub fn execute_install(opts: &InstallOptions) -> Result<String> {
    let all_agents = detect_agents()?;

    if all_agents.is_empty() {
        // Claude Code is configured through its own MCP command, not a config
        // file we edit, but its skill can still be installed.
        if !opts.list {
            if let Some(result) = install_claude_skill(opts) {
                return result;
            }
        }
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

    // Install or remove mode
    let action = if opts.remove {
        "Removing"
    } else {
        "Configuring"
    };
    let noun = if opts.remove { "from" } else { "for" };
    let mut lines = vec![format!(
        "{} cora MCP {} {} agent(s)…{}",
        action,
        noun,
        agents.len(),
        if opts.dry_run { " (dry run)" } else { "" }
    )];
    lines.push(String::new());

    for agent in &agents {
        let result = install_agent(agent, opts)?;
        lines.push(format!("{} {}", agent.name.bold(), result));
    }

    // Post-install validation
    if opts.validate && !opts.dry_run {
        lines.push(String::new());
        lines.push("Validating agent configs…".to_string());
        let mut errors = 0;
        for agent in &agents {
            match agent.format {
                ConfigFormat::Json | ConfigFormat::Jsonc => {
                    if let Err(e) = validate_json_config(&agent.config_path) {
                        lines.push(format!(
                            "  {} {} — INVALID: {}",
                            "✗ ".red(),
                            agent.config_path.display(),
                            e
                        ));
                        errors += 1;
                    } else {
                        lines.push(format!(
                            "  {} {} — valid JSON",
                            "✓ ".green(),
                            agent.config_path.display()
                        ));
                    }
                }
                ConfigFormat::Yaml => {
                    lines.push(format!(
                        "  {} {} — skipped (YAML validation not implemented)",
                        "⏭ ".dimmed(),
                        agent.config_path.display()
                    ));
                }
            }
        }
        if errors > 0 {
            lines.push(format!(
                "\n{} {errors} config(s) failed validation!",
                "⚠ ".yellow()
            ));
        }
    }

    if opts.agents.is_none() {
        if let Some(result) = install_claude_skill(opts) {
            lines.push(result?);
        }
    }

    lines.push(String::new());
    if opts.remove {
        lines.push("Done. Restart your AI agent to pick up the changes.".to_string());
    } else {
        lines.push("Done. Restart your AI agent to pick up the new MCP server.".to_string());
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> InstallOptions {
        InstallOptions {
            list: false,
            agents: None,
            dry_run: false,
            force: false,
            yes: true,
            remove: false,
            validate: false,
        }
    }

    fn skill_path(dir: &std::path::Path) -> PathBuf {
        dir.join("skills").join("cora-review").join("SKILL.md")
    }

    #[test]
    fn skill_is_written_then_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let path = skill_path(dir.path());
        let msg = install_skill_at(&path, &opts()).unwrap();
        assert!(msg.contains("installed"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_SKILL);
        assert!(
            install_skill_at(&path, &opts())
                .unwrap()
                .contains("up to date")
        );
    }

    #[test]
    fn skill_with_user_edits_is_kept_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let path = skill_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "my own version").unwrap();
        assert!(
            install_skill_at(&path, &opts())
                .unwrap()
                .contains("differs")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "my own version");
        let force = InstallOptions {
            force: true,
            ..opts()
        };
        install_skill_at(&path, &force).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CLAUDE_SKILL);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = skill_path(dir.path());
        let dry = InstallOptions {
            dry_run: true,
            ..opts()
        };
        assert!(
            install_skill_at(&path, &dry)
                .unwrap()
                .contains("would write")
        );
        assert!(!path.exists());
    }

    #[test]
    fn remove_deletes_the_skill() {
        let dir = tempfile::tempdir().unwrap();
        let path = skill_path(dir.path());
        install_skill_at(&path, &opts()).unwrap();
        let remove = InstallOptions {
            remove: true,
            ..opts()
        };
        assert!(
            install_skill_at(&path, &remove)
                .unwrap()
                .contains("removed")
        );
        assert!(!path.exists());
        assert!(
            install_skill_at(&path, &remove)
                .unwrap()
                .contains("no skill found")
        );
    }

    #[test]
    fn skill_names_the_four_tools() {
        for tool in [
            "cora.review_prepare",
            "cora.review_finalize",
            "cora.scan_prepare",
            "cora.scan_finalize",
        ] {
            assert!(CLAUDE_SKILL.contains(tool), "skill must mention {tool}");
        }
        assert!(CLAUDE_SKILL.starts_with("---\nname: cora-review\n"));
        assert!(CLAUDE_SKILL.contains("untrusted"));
    }
}
