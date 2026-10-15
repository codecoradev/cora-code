//! MCP (Model Context Protocol) server — exposes cora rules and config to AI coding agents.
//!
//! Allows Claude Code, Cursor, Copilot, Windsurf, etc. to query cora's quality
//! rules, project config, and review history in real-time.
//!
//! Transport: stdio (stdin/stdout JSON-RPC)

pub mod agent_tools;
pub mod protocol;
pub mod server;
pub mod tools;

use serde::{Deserialize, Serialize};

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct McpConfig {
    /// Enable MCP server.
    #[serde(default)]
    pub enabled: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for McpConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}
