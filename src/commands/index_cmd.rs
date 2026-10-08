//! `cora index` — thin dispatch over [`IndexSession`].
//!
//! Modes (checked in this order after an optional `--rebuild`): `--stats`,
//! `--prune`, `--watch`, otherwise a normal incremental index.

use anyhow::Result;
use colored::Colorize;

use crate::index::session::{ConfigSource, IndexSession};

/// Parsed `cora index` flags (verbose already merged with the global flag).
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptions {
    pub stats: bool,
    pub prune: bool,
    pub rebuild: bool,
    pub watch: bool,
    pub verbose: bool,
}

/// Debounce used when `index --watch` delegates to the `watch` implementation.
pub const WATCH_DEBOUNCE_MS: u64 = 500;

pub fn run_index(opts: &IndexOptions, config_path: Option<&str>) -> Result<()> {
    let mut session = IndexSession::open(ConfigSource::Full(config_path))?;

    if opts.rebuild {
        session.rebuild()?;
        eprintln!("{}", "Dropped existing index for project.".dimmed());
    }

    if opts.stats {
        print_stats(&session)?;
    } else if opts.prune {
        let deleted = session.prune()?;
        println!(
            "{}",
            format!("Pruned {deleted} deleted files from index.").green()
        );
    } else if opts.watch {
        super::watch::run_watch(&session, WATCH_DEBOUNCE_MS, false, None, opts.verbose)?;
    } else {
        run_once(&session, opts.verbose)?;
    }
    Ok(())
}

fn print_stats(session: &IndexSession) -> Result<()> {
    let summary = session.summary()?;
    println!("{}", "SYMBOL INDEX".cyan().bold());
    println!("{}", "────────────────────────────".dimmed());
    println!("  Total symbols:  {}", summary.total_symbols);
    println!("  Total files:    {}", summary.total_files);
    println!(
        "  Database size:  {}",
        crate::format_bytes(summary.db_size_bytes)
    );
    println!();
    println!("  {}", "By Kind".cyan());
    for (kind, count) in &summary.symbols_by_kind {
        println!("    {kind:<16} {count}");
    }
    println!();
    println!("  {}", "By Language".cyan());
    for (lang, count) in &summary.symbols_by_language {
        println!("    {lang:<16} {count}");
    }
    Ok(())
}

fn run_once(session: &IndexSession, verbose: bool) -> Result<()> {
    eprintln!("{}", "🔍 Indexing project...".cyan());
    let stats = session.index(verbose)?;
    if stats.files_indexed == 0 && stats.errors == 0 {
        // Incremental no-op: fingerprints all matched. Report the
        // STORED totals instead of a confusing zeros line (#522).
        eprintln!(
            "{}",
            format!(
                "✓ Index up to date ({} files unchanged)",
                stats.files_skipped
            )
            .green()
        );
        if let Ok(summary) = session.summary() {
            eprintln!(
                "{}",
                format!(
                    "   {} symbols across {} files",
                    summary.total_symbols, summary.total_files
                )
                .dimmed()
            );
        }
    } else {
        eprintln!(
            "{}",
            format!(
                "✅ Indexed {} symbols from {} files ({} skipped, {} errors)",
                stats.symbols_indexed, stats.files_indexed, stats.files_skipped, stats.errors
            )
            .green()
        );
    }
    if stats.files_excluded > 0 {
        eprintln!(
            "{}",
            format!(
                "   {} files excluded by ignore patterns",
                stats.files_excluded
            )
            .dimmed()
        );
    }
    eprintln!(
        "{}",
        format!(
            "   Database: {}",
            crate::data_dir::graph_db_path().display()
        )
        .dimmed()
    );
    Ok(())
}
