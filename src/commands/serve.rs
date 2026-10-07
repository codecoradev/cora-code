//! `cora serve` — start MCP server with automatic reindex on startup.

/// Execute the serve command: auto-reindex the current project, then start the MCP server.
pub fn execute_serve() -> anyhow::Result<()> {
    // 1. Auto-reindex current project (incremental — skips unchanged files)
    let (conn, _project_id, project_root) =
        crate::engine::index_bridge::IndexBridge::open_or_create_cwd()?.into_strict_parts()?;

    let skip_patterns = crate::index::prepare_index_config(None);
    let stats = crate::index::index_project_with_skip(
        &conn,
        &project_root,
        false,
        skip_patterns.as_deref(),
    )?;

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
