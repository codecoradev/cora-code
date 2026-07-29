//! `cora serve` — start MCP server with automatic reindex on startup.

/// Execute the serve command: auto-reindex the current project, then start the MCP server.
pub fn execute_serve() -> anyhow::Result<()> {
    // 1. Auto-reindex current project (incremental — skips unchanged files)
    let project_root = std::env::current_dir()?;
    let project_root =
        crate::index::resolve_project_root(&project_root).unwrap_or(project_root.clone());

    let conn = crate::index::open_global_index()?;
    let _project_id = crate::index::ensure_project(&conn, &project_root)?;

    let stats = crate::index::index_project(&conn, &project_root, false)?;

    if stats.files_indexed > 0 {
        eprintln!(
            "  Indexed {} files ({} symbols, {} skipped)",
            stats.files_indexed, stats.symbols_indexed, stats.files_skipped
        );
    } else {
        eprintln!("  Index up to date ({} files scanned, {} skipped)", stats.files_scanned, stats.files_skipped);
    }

    // 2. Start MCP server (same as `cora mcp`)
    crate::mcp::server::run_server()?;

    Ok(())
}
