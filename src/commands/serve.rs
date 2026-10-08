//! `cora serve` — start MCP server with automatic reindex on startup.

use crate::index::session::{ConfigSource, IndexSession};

/// Execute the serve command: auto-reindex the current project, then start the MCP server.
///
/// `config_path` is the global `--config` flag; the session honors it.
pub fn execute_serve(config_path: Option<&str>) -> anyhow::Result<()> {
    // 1. Auto-reindex current project (incremental — skips unchanged files)
    let session = IndexSession::open(ConfigSource::Full(config_path))?;
    let stats = session.index(false)?;
    drop(session);

    if stats.files_indexed > 0 {
        eprintln!(
            "  Indexed {} files ({} symbols, {} skipped)",
            stats.files_indexed, stats.symbols_indexed, stats.files_skipped
        );
    } else {
        eprintln!(
            "  Index up to date ({} files scanned, {} skipped)",
            stats.files_scanned, stats.files_skipped
        );
    }

    // 2. Start MCP server (same as `cora mcp`)
    crate::mcp::server::run_server()?;

    Ok(())
}
