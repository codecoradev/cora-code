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
