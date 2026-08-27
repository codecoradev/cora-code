use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

mod commands;
mod config;
mod data_dir;
mod embed;
mod engine;
mod error;
mod formatters;
mod git;
mod hook;
mod index;
mod mcp;
mod progress;

use index::schema;

use commands::{
    auth, commit_cmd, completion, config_cmd, debt, hook_cmd, init, profile, providers, review,
    scan, upload,
};
use config::loader;
use engine::memory;
use formatters::OutputFormat;

/// Cora — AI Code Review CLI with BYOK (Bring Your Own Key)
///
/// Review diffs and scan projects using any OpenAI-compatible LLM.
/// Configure via .cora.yaml, CLI flags, or environment variables.
#[derive(Parser, Debug)]
#[clap(
    name = "cora",
    version,
    about = "CLI-first AI code review — BYOK, diff/scan/branch, pre-commit hooks",
    long_version = concat!(env!("CARGO_PKG_VERSION"), "\n", env!("CARGO_PKG_REPOSITORY"))
)]
struct Cli {
    /// Global options
    #[command(flatten)]
    global: GlobalOptions,

    /// Subcommand
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args, Debug, Clone)]
struct GlobalOptions {
    /// Path to config file (default: auto-discover .cora.yaml)
    #[clap(long, global = true, env = "CORA_CONFIG")]
    pub config: Option<String>,

    /// Output format: pretty, json, compact, sarif
    #[clap(long, global = true, value_parser = ["pretty", "json", "compact", "sarif"], env = "CORA_FORMAT")]
    pub format: Option<String>,

    /// Disable colored output
    #[clap(long, global = true, env = "CORA_NO_COLOR")]
    pub no_color: bool,

    /// LLM provider name (e.g. openai, anthropic, ollama)
    #[clap(long, global = true, env = "CORA_PROVIDER")]
    pub provider: Option<String>,

    /// LLM model name (e.g. gpt-4o-mini, claude-3-haiku)
    #[clap(long, global = true, env = "CORA_MODEL")]
    pub model: Option<String>,

    /// API base URL
    #[clap(long, global = true, env = "CORA_BASE_URL")]
    pub base_url: Option<String>,

    /// API key (or set `CORA_API_KEY` env var for CI, or use `cora auth login`)
    #[clap(long, global = true, env = "CORA_API_KEY")]
    pub api_key: Option<String>,

    /// Enable verbose logging
    #[clap(long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Build or update the symbol index for code intelligence
    Index {
        /// Show index statistics instead of building
        #[clap(long)]
        stats: bool,

        /// Prune deleted files from index
        #[clap(long)]
        prune: bool,

        /// Rebuild index from scratch (drop existing)
        #[clap(long)]
        rebuild: bool,

        /// Watch for file changes and auto-update index
        #[clap(long)]
        watch: bool,

        /// Verbose output
        #[clap(long, short)]
        verbose: bool,
    },

    /// Search the symbol index for code intelligence
    Explore {
        /// Search query (symbol name or keyword)
        query: Option<String>,

        /// Filter by symbol kind (function, struct, enum, trait, etc.)
        #[clap(long)]
        kind: Option<String>,

        /// Filter by file path prefix
        #[clap(long)]
        file: Option<String>,

        /// Filter by language
        #[clap(long)]
        language: Option<String>,

        /// Maximum results
        #[clap(long, default_value = "50")]
        limit: usize,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Find all callers of a symbol (who calls this?)
    Callers {
        /// Symbol name to search for
        symbol: String,

        /// Maximum results
        #[clap(long, default_value = "50")]
        limit: usize,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Analyze the impact of changing a symbol (what breaks?)
    Impact {
        /// Symbol name to analyze
        symbol: String,

        /// Traversal depth (how many levels up the call graph)
        #[clap(long, default_value = "3")]
        depth: u32,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Trace execution paths through the codebase
    Trace {
        /// Symbol name to start tracing from
        symbol: String,

        /// Trace direction: outgoing (callees) or incoming (callers)
        #[clap(long, value_parser = ["outgoing", "incoming"], default_value = "outgoing")]
        direction: String,

        /// Traversal depth (max hops)
        #[clap(long, default_value = "3")]
        depth: u32,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Show architecture overview (modules, edge types, structure)
    Arch {
        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Hybrid search: FTS5 + vector + graph → RRF fusion
    Brain {
        /// Search query
        query: Vec<String>,

        /// Maximum results (default: 20)
        #[clap(long, default_value = "20")]
        limit: usize,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Find tests affected by changed files
    Affected {
        /// Changed files (space-separated). If empty, reads from git diff.
        files: Vec<String>,

        /// Read changed files from stdin (pipe from git diff --name-only)
        #[clap(long)]
        stdin: bool,

        /// Test file glob pattern (default: *test*, *spec*)
        #[clap(long)]
        filter: Option<String>,

        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Review staged changes, generate commit message, and commit
    Commit {
        /// YOLO mode — auto-commit without prompts
        #[clap(long)]
        yolo: bool,

        /// Force commit even if quality gate fails
        #[clap(long)]
        force: bool,

        /// Skip review, only generate commit message
        #[clap(long)]
        no_review: bool,

        /// Always open editor to edit message
        #[clap(long)]
        edit: bool,

        /// Stream LLM response in real-time
        #[clap(long)]
        stream: bool,

        /// Suppress all output except the result
        #[clap(long, short)]
        quiet: bool,
    },

    /// Review a git diff (staged, unpushed, or branch)
    Review {
        /// Review staged changes (git diff --cached)
        #[clap(long)]
        staged: bool,

        /// Review unpushed changes (HEAD vs @{u})
        #[clap(long)]
        unpushed: bool,

        /// Review changes vs a base branch
        #[clap(long)]
        base: Option<String>,

        /// Review changes from a git commit ref (e.g. HEAD, HEAD~3..HEAD, abc123)
        #[clap(long)]
        commit: Option<String>,

        /// Read diff from a file instead of git
        #[clap(long)]
        diff_file: Option<String>,

        /// Review unstaged changes (working tree)
        #[clap(long)]
        unstaged: bool,

        /// Override max diff size in chars (default: 50000 from config)
        #[clap(long, name = "CHARS")]
        max_diff_size: Option<usize>,

        /// Stream LLM response tokens in real-time
        #[clap(long)]
        stream: bool,

        /// Disable auto-chunking for large diffs (auto-chunk is enabled by default)
        #[clap(long)]
        no_auto_chunk: bool,

        /// Output structured NDJSON progress events to stderr
        #[clap(long)]
        progress: bool,

        /// Suppress all output except the formatted review result
        #[clap(long, short)]
        quiet: bool,

        /// Write formatted output to a file instead of stdout
        #[clap(long, value_name = "PATH")]
        output_file: Option<String>,

        /// Filter results by minimum severity (info, minor, major, critical)
        #[clap(long, value_parser = ["info", "minor", "major", "critical"])]
        severity: Option<String>,

        /// Disable review caching
        #[clap(long)]
        no_cache: bool,

        /// CI mode: skip diff size limit, exit 2 if any findings
        #[clap(long)]
        ci: bool,

        /// Upload SARIF output to GitHub Code Scanning after review
        /// (implies --format sarif)
        #[clap(long)]
        upload: bool,

        /// GitHub repository for upload (default: from git remote origin)
        #[clap(long, env = "GITHUB_REPOSITORY")]
        repo: Option<String>,

        /// GitHub ref for upload (default: current branch)
        #[clap(long, env = "GITHUB_REF")]
        ref_name: Option<String>,

        /// GitHub token for upload (default: `GITHUB_TOKEN` env var)
        #[clap(long, env = "GITHUB_TOKEN")]
        token: Option<String>,

        /// Recall project patterns from Uteke before review (requires `uteke` CLI)
        #[clap(long)]
        memory: bool,

        /// Save review findings to Uteke after review (implies --memory)
        #[clap(long)]
        learn: bool,
    },

    /// Scan a project directory for issues
    Scan {
        /// Root directory to scan (default: current directory)
        #[clap(long)]
        path: Option<String>,

        /// Include glob patterns (e.g. "src/**/*.rs")
        #[clap(long)]
        include: Vec<String>,

        /// Exclude glob patterns (e.g. "vendor/**")
        #[clap(long)]
        exclude: Vec<String>,

        /// Additional file extensions to scan
        #[clap(long)]
        extensions: Vec<String>,

        /// Only scan files changed since last scan (uses ~/.cora/scan-cache.json)
        #[clap(long)]
        incremental: bool,

        /// Focus areas for review (overrides config)
        #[clap(long)]
        focus: Vec<String>,

        /// Maximum files per LLM batch (default: 20). Lower this to work around
        /// provider token limits or rate-limit errors on large scans.
        #[clap(long, value_name = "N", default_value_t = 20)]
        batch_files: usize,

        /// Abort the entire scan when a batch fails to parse instead of
        /// skipping it and continuing (default: skip and continue).
        #[clap(long)]
        no_continue_on_batch_error: bool,
    },

    /// Manage quality profiles (preset rule sets)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Upload a SARIF file to GitHub Code Scanning
    UploadSarif {
        /// Path to SARIF file to upload (default: reads from stdin)
        file: Option<String>,

        /// GitHub repository (default: from git remote origin)
        #[clap(long, env = "GITHUB_REPOSITORY")]
        repo: Option<String>,

        /// GitHub ref (default: current branch)
        #[clap(long, env = "GITHUB_REF")]
        ref_name: Option<String>,

        /// GitHub token (default: `GITHUB_TOKEN` env var)
        #[clap(long, env = "GITHUB_TOKEN")]
        token: Option<String>,
    },

    /// Create a .cora.yaml config file and install pre-commit hook
    Init {
        /// Overwrite existing config file
        #[clap(long)]
        force: bool,

        /// Skip pre-commit hook installation
        #[clap(long)]
        no_hook: bool,
    },

    /// Manage pre-commit git hooks
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },

    /// Manage API key authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },

    /// View or set configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// List detected/available LLM providers
    Providers,

    /// Generate shell completion scripts
    Completion {
        /// Shell name: bash, zsh, fish, or powershell
        shell: String,
    },

    /// Start MCP server (Model Context Protocol for AI agents)
    Mcp,

    /// Manage review findings (list, dismiss, reopen, stats)
    Findings {
        #[command(subcommand)]
        action: commands::findings::FindingsAction,
    },

    /// Show tech debt report from review history
    Debt {
        /// Output as JSON
        #[clap(long)]
        json: bool,

        /// Show quality score trend graph
        #[clap(long)]
        trend: bool,

        /// Only show data since a date (YYYY-MM-DD) or git tag
        #[clap(long)]
        since: Option<String>,

        /// Filter by branch name
        #[clap(long)]
        branch: Option<String>,

        /// Output shields.io badge JSON
        #[clap(long)]
        badge: bool,

        /// Show estimated debt fix time
        #[clap(long)]
        estimate: bool,
    },
    /// Watch for file changes and auto-reindex (standalone real-time watcher)
    Watch {
        /// Debounce window in milliseconds (default: 500)
        #[clap(long, default_value = "500")]
        debounce: u64,
        /// Only trigger on git-tracked files
        #[clap(long)]
        git_only: bool,
        /// Glob filter pattern (e.g. 'src/**/*.rs')
        #[clap(long)]
        filter: Option<String>,
    },
    /// Auto-detect and configure AI coding agents for Cora MCP
    Install {
        /// List detected agents without installing
        #[clap(long)]
        list: bool,
        /// Specific agents to install (comma-separated, e.g. "cline,cursor")
        #[clap(long)]
        agents: Option<String>,
        /// Dry run — show what would be changed
        #[clap(long)]
        dry_run: bool,
        /// Overwrite existing cora entry
        #[clap(long)]
        force: bool,
        /// Install ALL detected agents (non-interactive)
        #[clap(long, short)]
        yes: bool,
        /// Remove cora MCP entry from detected agents (uninstall)
        #[clap(long)]
        remove: bool,
        /// Validate agent configs after install/remove
        #[clap(long)]
        validate: bool,
    },

    /// Detect dead code — functions/methods with no callers
    DeadCode {
        /// Include test functions (test_*, _test) in results
        #[clap(long)]
        include_tests: bool,
        /// Include public API surface (pub/export items) in results.
        /// By default they are skipped: they are consumed externally, so
        /// missing internal callers does not make them dead (#520).
        #[clap(long)]
        include_pub: bool,
        /// Minimum lines of code (filter out tiny functions)
        #[clap(long)]
        min_lines: Option<u32>,
        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Query the code graph with simple patterns (e.g. "main -> *", "* -> authenticate")
    Query {
        /// Graph query pattern (e.g. "main -> *", "* -> main", "MyStruct")
        query: String,
        /// Maximum results
        #[clap(long, default_value = "50")]
        limit: usize,
        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// List detected HTTP routes (Axum, Actix, Express, FastAPI, Flask, Go)
    Routes {
        /// Filter by HTTP method (e.g. GET, POST). Case-insensitive.
        #[clap(long)]
        method: Option<String>,
        /// Filter by path prefix (e.g. /api)
        #[clap(long)]
        prefix: Option<String>,
        /// Output as JSON
        #[clap(long)]
        json: bool,
    },

    /// Start MCP server with auto-reindex on startup
    Serve,

    /// Check for updates and self-upgrade the cora binary
    Upgrade {
        /// Skip confirmation prompt (useful for CI/automation)
        #[clap(long)]
        yes: bool,

        /// Only check if an update is available, do not download
        #[clap(long)]
        check: bool,
    },
}

#[derive(Subcommand, Debug)]
enum HookAction {
    /// Install the pre-commit hook
    Install,
    /// Uninstall the pre-commit hook
    Uninstall,
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Save an API key to local config
    Login {
        /// Provider name (e.g. openai, zai, anthropic, ollama) — skips interactive selection
        #[clap(long)]
        provider: Option<String>,
        /// API key — skips interactive prompt
        #[clap(long)]
        api_key: Option<String>,
        /// Model name (e.g. glm-5.1, gpt-4o-mini) — used with --provider
        #[clap(long)]
        model: Option<String>,
        /// Base URL — used with --provider for custom endpoints
        #[clap(long)]
        base_url: Option<String>,
        /// Skip confirmation when overwriting existing key
        #[clap(long)]
        force: bool,
    },
    /// Check if an API key is configured
    Status,
    /// Remove the stored API key
    Remove,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Show the current resolved configuration
    Show {
        /// Show only global config (~/.cora/config.yaml)
        #[clap(long, conflicts_with = "project")]
        global: bool,
        /// Show only project config (.cora.yaml)
        #[clap(long, conflicts_with = "global")]
        project: bool,
    },
    /// Set a configuration value (keys: model, provider, `base_url`, format, severity)
    Set {
        /// Configuration key to set
        key: String,
        /// Value to assign
        value: String,
        /// Write to global config (~/.cora/config.yaml) instead of project .cora.yaml
        #[clap(long)]
        global: bool,
    },
    /// Validate the current configuration and report status
    Validate,
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    /// List available quality profiles
    List,
    /// Show details of a specific profile
    Show {
        /// Profile name (e.g. security-first, rust-strict)
        name: String,
    },
    /// Validate a custom profile YAML file
    Validate {
        /// Path to the profile YAML file
        path: String,
    },
}

/// Format bytes as human-readable string.
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Set up logging based on verbosity
    let log_level = if cli.global.verbose {
        Level::DEBUG
    } else {
        Level::WARN
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .with_writer(std::io::stderr) // logs/warns must go to stderr, not stdout
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("cora=warn")),
        )
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    // Handle --no-color
    if cli.global.no_color {
        colored::control::set_override(false);
    }

    // Background update check (non-blocking, cached 24h).
    // Skip for `upgrade` command itself and when `--no-update-check` is set.
    let skip_update_check = matches!(cli.command, Command::Upgrade { .. })
        || std::env::var("CORA_NO_UPDATE_CHECK")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
    if !skip_update_check {
        commands::update_check::spawn_background_check(true);
    }

    // Dispatch based on subcommand
    let exit_code = match cli.command {
        Command::Index {
            stats: show_stats,
            prune,
            rebuild,
            watch,
            verbose,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            if rebuild {
                // Delete all data for this project via CASCADE
                schema::delete_project(&conn, project_id)?;
                eprintln!("{}", "Dropped existing index for project.".dimmed());
                // Re-register the project (gets a fresh project_id)
                let _fresh_id =
                    schema::get_or_create_project(&conn, &project_root.to_string_lossy())?;
            }

            if show_stats {
                let summary = index::index_stats(&conn, project_id)?;
                println!("{}", "SYMBOL INDEX".cyan().bold());
                println!("{}", "────────────────────────────".dimmed());
                println!("  Total symbols:  {}", summary.total_symbols);
                println!("  Total files:    {}", summary.total_files);
                println!("  Database size:  {}", format_bytes(summary.db_size_bytes));
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
            } else if prune {
                let deleted = index::prune_deleted(&conn, project_id, &project_root)?;
                println!(
                    "{}",
                    format!("Pruned {deleted} deleted files from index.").green()
                );
            } else if watch {
                // Initial index
                eprintln!("{}", "🔍 Initial index...".cyan());
                let stats =
                    index::index_project(&conn, &project_root, verbose || cli.global.verbose)?;
                eprintln!(
                    "{}",
                    format!(
                        "✅ Indexed {} symbols. Watching for changes... (Ctrl+C to stop)",
                        stats.symbols_indexed
                    )
                    .green()
                );

                // Poll loop: re-index changed files every 2 seconds
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    let stats = index::index_project(&conn, &project_root, false)?;
                    if stats.files_indexed > 0 {
                        eprintln!(
                            "{}",
                            format!(
                                "🔄 Updated {} files, {} symbols",
                                stats.files_indexed, stats.symbols_indexed
                            )
                            .cyan()
                        );
                    }
                }
            } else {
                // Load config for config-hash invalidation + brain embedding backend
                let config = crate::config::loader::load_config(
                    cli.global.config.as_deref(),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .ok();
                // Exclusion patterns = review's ignore.files + index.skip_files
                // so dead-code/index scanners respect the same ignores (#521).
                let skip_patterns: Option<Vec<String>> = config.as_ref().map(|c| {
                    let mut pats = c.ignore.files.clone();
                    pats.extend(c.rules_config.index_skip_files.iter().cloned());
                    pats.dedup();
                    pats
                });

                // Resolve embedding backend from brain config
                let brain_mode = config
                    .as_ref()
                    .map(|c| c.brain.embedding.to_string())
                    .unwrap_or_else(|| "auto".to_string());
                crate::embed::resolve_backend(&brain_mode);

                eprintln!("{}", "🔍 Indexing project...".cyan());
                let stats = index::index_project_with_skip(
                    &conn,
                    &project_root,
                    verbose || cli.global.verbose,
                    skip_patterns.as_deref(),
                )?;
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
                    if let Ok(summary) = index::index_stats(&conn, project_id) {
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
                            stats.symbols_indexed,
                            stats.files_indexed,
                            stats.files_skipped,
                            stats.errors
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
            }
            0
        }

        Command::Explore {
            query,
            kind,
            file,
            language,
            limit,
            json,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();

            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }

            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            let sym_kind = kind.as_deref().map(index::SymbolKind::from_str);

            let q = index::SymbolQuery {
                text: query,
                kind: sym_kind,
                file_prefix: file,
                language,
                limit,
            };

            let results = index::search(&conn, project_id, &q)?;

            if json {
                let json_results: Vec<serde_json::Value> = results
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
                println!("{}", serde_json::to_string_pretty(&json_results)?);
            } else if results.is_empty() {
                eprintln!("{}", "No symbols found.".yellow());
            } else {
                println!("{}", format!("Found {} symbols:", results.len()).cyan());
                println!(
                    "{}",
                    "───────────────────────────────────────────────".dimmed()
                );
                for r in &results {
                    println!(
                        "  {} {} {}:{}",
                        r.symbol.kind.as_str().blue(),
                        r.symbol.name.white().bold(),
                        r.symbol.file.dimmed(),
                        r.symbol.line
                    );
                    if !r.symbol.signature.is_empty() {
                        println!(
                            "    {} {}",
                            "→".dimmed(),
                            r.symbol.signature.trim().dimmed()
                        );
                    }
                }
            }
            0
        }

        Command::Callers {
            symbol,
            limit,
            json,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;
            let callers = index::graph::find_callers(&conn, project_id, &symbol, limit)?;

            // Cross-project fallback: if no callers in current project,
            // search across all indexed projects.
            let cross_project_results = if callers.is_empty() {
                let cp = index::graph::find_callers_cross_project(&conn, &symbol, limit)?;
                if !cp.is_empty() {
                    if !json {
                        eprintln!(
                            "{}",
                            format!(
                                "No callers in current project. Found {} in other project(s):",
                                cp.len()
                            )
                            .yellow()
                        );
                    }
                    Some(cp)
                } else {
                    None
                }
            } else {
                None
            };

            if json {
                if let Some(ref cp) = cross_project_results {
                    println!("{}", serde_json::to_string_pretty(cp)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&callers)?);
                }
            } else if callers.is_empty() && cross_project_results.is_some() {
                let cp = cross_project_results.as_ref().unwrap();
                println!(
                    "{}",
                    format!("Callers of '{symbol}' ({}):", cp.len()).cyan()
                );
                println!("{}", "─".repeat(50).dimmed());
                for c in cp {
                    println!(
                        "  {} {}:{}  {}",
                        c.caller.white().bold(),
                        c.file.dimmed(),
                        c.line,
                        c.project_root.dimmed().italic(),
                    );
                }
            } else if callers.is_empty() {
                eprintln!("{}", format!("No callers found for '{symbol}'.").yellow());
            } else {
                println!(
                    "{}",
                    format!("Callers of '{symbol}' ({}):", callers.len()).cyan()
                );
                println!("{}", "─".repeat(50).dimmed());
                for c in &callers {
                    println!(
                        "  {} {}:{}",
                        c.caller.white().bold(),
                        c.file.dimmed(),
                        c.line
                    );
                }
            }
            0
        }

        Command::Impact {
            symbol,
            depth,
            json,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run 'cora index' first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;
            let impact = index::graph::impact_analysis(&conn, project_id, &symbol, depth)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&impact)?);
            } else if impact.is_empty() {
                eprintln!("{}", format!("No impact found for '{}'.", symbol).yellow());
            } else {
                println!(
                    "{}",
                    format!(
                        "Impact of changing '{}' ({} affected):",
                        symbol,
                        impact.len()
                    )
                    .cyan()
                );
                println!("{}", "\u{2500}".repeat(50).dimmed());
                let mut prev_depth = 0;
                for node in &impact {
                    if node.depth != prev_depth {
                        prev_depth = node.depth;
                        println!("  {}", format!("depth {}", node.depth).blue().bold());
                    }
                    println!(
                        "    {} {}:{}",
                        node.symbol.white(),
                        node.file.dimmed(),
                        node.line
                    );
                }
            }
            0
        }

        Command::Trace {
            symbol,
            direction,
            depth,
            json,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            let dir = match direction.as_str() {
                "incoming" => index::graph::TraceDirection::Incoming,
                _ => index::graph::TraceDirection::Outgoing,
            };

            let nodes = index::graph::trace_path(&conn, project_id, &symbol, depth, dir)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&nodes)?);
            } else if nodes.is_empty() {
                eprintln!("{}", format!("No trace results for '{symbol}'.").yellow());
            } else {
                let dir_label = match dir {
                    index::graph::TraceDirection::Outgoing => "calls",
                    index::graph::TraceDirection::Incoming => "called by",
                };
                println!(
                    "{}",
                    format!(
                        "Trace from '{}' ({}, {} nodes):",
                        symbol,
                        dir_label,
                        nodes.len()
                    )
                    .cyan()
                );
                println!("{}", "─".repeat(60).dimmed());
                let mut prev_depth = 0u32;
                for node in &nodes {
                    if node.depth != prev_depth {
                        prev_depth = node.depth;
                        println!("  {}", format!("depth {}", node.depth).blue().bold());
                    }
                    let indent = "  ".repeat(node.depth as usize + 1);
                    let kind_tag = format!("[{}]", node.kind).dimmed();
                    println!(
                        "  {}{} {} {}:{}",
                        indent,
                        node.symbol.white().bold(),
                        kind_tag,
                        node.file.dimmed(),
                        node.line
                    );
                }
            }
            0
        }

        Command::Arch { json } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            let overview = index::graph::arch_overview(&conn, project_id)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&overview)?);
            } else {
                println!("{}", "ARCHITECTURE OVERVIEW".cyan().bold());
                println!("{}", "─".repeat(50).dimmed());
                println!();
                println!(
                    "  {}  {}  {}",
                    "MODULE".bold(),
                    "SYMBOLS".bold(),
                    "EDGE TYPES".bold()
                );
                println!("  {}", "─".repeat(45).dimmed());
                for m in &overview.modules {
                    println!(
                        "  {:<30} {:>6}  {:>10}",
                        m.name, m.symbol_count, m.edge_types
                    );
                }
                println!();
                if !overview.edge_counts.is_empty() {
                    println!("  {}", "EDGE DISTRIBUTION".cyan().bold());
                    println!("  {}", "─".repeat(45).dimmed());
                    for (kind, count) in &overview.edge_counts {
                        println!("  {:<20} {:>6}", kind, count);
                    }
                }
            }
            0
        }

        Command::Brain { query, limit, json } => {
            let query_str = query.join(" ");
            if query_str.trim().is_empty() {
                eprintln!("{}", "Usage: cora brain <query>".yellow());
                std::process::exit(1);
            }

            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            // Resolve embedding backend from config for query embedding
            let brain_mode = crate::config::loader::load_config(
                cli.global.config.as_deref(),
                None,
                None,
                None,
                None,
                false,
            )
            .ok()
            .map(|c| c.brain.embedding.to_string())
            .unwrap_or_else(|| "auto".to_string());
            crate::embed::resolve_backend(&brain_mode);

            let results = index::brain::brain_search(&conn, project_id, &query_str, limit)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else if results.is_empty() {
                println!("{}", "No results found.".dimmed());
            } else {
                println!(
                    "{} {} results for '{}'",
                    "🧠".magenta(),
                    results.len().to_string().bold(),
                    query_str
                );
                println!("{}", "─".repeat(60).dimmed());
                for r in &results {
                    let signals = r.signals.join(",");
                    let score = format!("{:.4}", r.score);
                    println!(
                        "  {:<40} {:<15} {} {}",
                        format!("{} ({})", r.name, r.file).green(),
                        format!("[{}]", r.kind).dimmed(),
                        format!("score={}", score).cyan(),
                        format!("signals={}", signals).dimmed(),
                    );
                }
            }
            0
        }

        Command::Affected {
            files,
            stdin,
            filter,
            json,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let db_path = crate::data_dir::graph_db_path();
            if !db_path.exists() {
                eprintln!("{}", "No index found. Run `cora index` first.".yellow());
                std::process::exit(1);
            }
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            // Gather changed files
            let mut changed: Vec<String> = files;
            if stdin {
                use std::io::BufRead;
                let stdin = std::io::stdin();
                for line in stdin.lock().lines().map_while(Result::ok) {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        changed.push(trimmed);
                    }
                }
            }

            if changed.is_empty() {
                // Fallback: get from git diff
                let output = std::process::Command::new("git")
                    .args(["diff", "--name-only", "HEAD"])
                    .output()?;
                let stdout = String::from_utf8_lossy(&output.stdout);
                changed = stdout
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
            }

            if changed.is_empty() {
                eprintln!("{}", "No changed files detected.".yellow());
                std::process::exit(0);
            }

            // Default test patterns
            let patterns: Vec<String> = filter.map(|f| vec![f]).unwrap_or_else(|| {
                vec![
                    "test".to_string(),
                    "spec".to_string(),
                    "_test".to_string(),
                    "_spec".to_string(),
                ]
            });

            // Find test files that import/reference changed source files
            let mut affected_tests: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // Strategy 1: Find symbols in changed files, then find callers that are in test files
            // Batch: fetch all symbols for all changed files in a single query
            let all_symbols: Vec<String> = {
                let placeholders: String =
                    changed.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let n = changed.len() + 1;
                let sql = format!(
                    "SELECT DISTINCT name FROM symbols WHERE file IN ({placeholders}) AND project_id = ?{n}"
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = changed
                    .iter()
                    .map(|f| Box::new(f.clone()) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                params.push(Box::new(project_id));
                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    params.iter().map(|p| p.as_ref()).collect();
                let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))?;
                rows.filter_map(|r| r.ok()).collect()
            };

            // Deduplicate symbols and resolve callers with a single set-based query
            {
                let mut seen_syms: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for sym_name in all_symbols {
                    if seen_syms.insert(sym_name.clone()) {
                        let callers =
                            index::graph::find_callers(&conn, project_id, &sym_name, 100)?;
                        for caller in callers {
                            if patterns.iter().any(|p| caller.file.contains(p.as_str())) {
                                affected_tests.insert(caller.file.clone());
                            }
                        }
                    }
                }
            }

            // Strategy 2: Direct test file name convention (mod_test.rs, foo_test.go)
            // Prepare statement once before the loop
            let mut stmt = conn.prepare(
                "SELECT DISTINCT path FROM files WHERE path LIKE ?1 AND project_id = ?2",
            )?;
            for file in &changed {
                // For Rust: src/foo.rs → tests/foo.rs or src/foo.rs → src/foo_test.rs
                let stem = file
                    .rsplit('/')
                    .next()
                    .unwrap_or(file)
                    .rsplit('.')
                    .next()
                    .unwrap_or("");
                let test_patterns = [
                    format!("{stem}_test.rs"),
                    format!("tests/{stem}.rs"),
                    format!("test_{stem}.rs"),
                    format!("{stem}_test.go"),
                    format!("{stem}_test.py"),
                    format!("test_{stem}.py"),
                    format!("{stem}.test.ts"),
                    format!("{stem}.spec.ts"),
                ];
                for tp in &test_patterns {
                    let pattern = format!("%{tp}");
                    let rows = stmt.query_map(rusqlite::params![pattern, project_id], |row| {
                        row.get::<_, String>(0)
                    })?;
                    for f in rows.map_while(Result::ok) {
                        affected_tests.insert(f);
                    }
                }
            }

            let mut sorted: Vec<String> = affected_tests.into_iter().collect();
            sorted.sort();

            if json {
                println!("{}", serde_json::to_string_pretty(&sorted)?);
            } else if sorted.is_empty() {
                eprintln!("{}", "No affected test files found.".yellow());
            } else {
                println!(
                    "{}",
                    format!("Affected test files ({}):", sorted.len()).cyan()
                );
                println!("{}", "\u{2500}".repeat(50).dimmed());
                for f in &sorted {
                    println!("  {}", f.white().bold());
                }
            }
            0
        }

        Command::Commit {
            yolo,
            force,
            no_review,
            edit,
            stream,
            quiet,
        } => {
            let config = loader::load_config(
                cli.global.config.as_deref(),
                cli.global.provider.as_deref(),
                cli.global.model.as_deref(),
                cli.global.base_url.as_deref(),
                cli.global.format.as_deref(),
                cli.global.no_color,
            )?;
            let llm_config = loader::build_llm_config(&config, cli.global.api_key.as_deref())?;
            let opts = commit_cmd::CommitOptions {
                yolo,
                force,
                no_review,
                edit,
                stream,
                quiet,
            };
            commit_cmd::execute_commit(&config, &llm_config, &opts).await?
        }
        Command::Review {
            staged,
            unpushed,
            base,
            commit,
            diff_file,
            unstaged,
            max_diff_size,
            stream,
            no_auto_chunk,
            progress,
            quiet,
            output_file,
            severity,
            no_cache,
            ci,
            upload,
            repo,
            ref_name,
            token,
            memory,
            learn,
        } => {
            cmd_review(
                &cli.global,
                ReviewOpts {
                    staged,
                    unpushed,
                    base,
                    commit,
                    diff_file,
                    unstaged,
                    max_diff_size,
                    stream,
                    no_auto_chunk,
                    progress,
                    quiet,
                    output_file,
                    severity,
                    upload,
                    repo,
                    ref_name,
                    token,
                    no_cache,
                    ci,
                    memory,
                    learn,
                },
            )
            .await?
        }
        Command::Scan {
            path,
            include,
            exclude,
            extensions,
            incremental,
            focus,
            batch_files,
            no_continue_on_batch_error,
        } => {
            cmd_scan(
                &cli.global,
                ScanOpts {
                    path,
                    include,
                    exclude,
                    extensions,
                    incremental,
                    focus,
                    batch_files,
                    continue_on_batch_error: !no_continue_on_batch_error,
                },
            )
            .await?
        }
        Command::Profile { action } => {
            match action {
                ProfileAction::List => profile::execute_profile_list()?,
                ProfileAction::Show { name } => profile::execute_profile_show(&name)?,
                ProfileAction::Validate { path } => profile::execute_profile_validate(&path)?,
            }
            0
        }
        Command::UploadSarif {
            file,
            repo,
            ref_name,
            token,
        } => {
            let opts = upload::UploadOptions {
                file,
                repo,
                ref_name,
                token,
            };
            upload::execute_upload(&opts).await?
        }
        Command::Init { force, no_hook } => {
            if force {
                init::execute_init_force(no_hook)?;
            } else {
                init::execute_init(no_hook)?;
            }
            0
        }
        Command::Hook { action } => match action {
            HookAction::Install => {
                hook_cmd::execute_hook_install()?;
                0
            }
            HookAction::Uninstall => {
                hook_cmd::execute_hook_uninstall()?;
                0
            }
        },
        Command::Auth { action } => {
            match action {
                AuthAction::Login {
                    provider,
                    api_key,
                    model,
                    base_url,
                    force,
                } => {
                    auth::execute_auth_login(
                        provider.as_deref(),
                        api_key.as_deref(),
                        model.as_deref(),
                        base_url.as_deref(),
                        force,
                    )?;
                }
                AuthAction::Status => auth::execute_auth_status()?,
                AuthAction::Remove => auth::execute_auth_remove()?,
            }
            0
        }
        Command::Config { action } => match action {
            ConfigAction::Show { global, project } => {
                config_cmd::execute_config_show(global, project, cli.global.config.as_deref())?;
                0
            }
            ConfigAction::Set { key, value, global } => {
                config_cmd::execute_config_set(&key, &value, global)?;
                0
            }
            ConfigAction::Validate => {
                config_cmd::execute_config_validate(cli.global.config.as_deref())?
            }
        },
        Command::Providers => {
            providers::execute_providers();
            0
        }
        Command::Completion { shell } => {
            completion::execute_completion(&shell)?;
            0
        }
        Command::Mcp => {
            mcp::server::run_server()?;
            0
        }
        Command::Findings { action } => commands::findings::execute_findings(&action)?,
        Command::Debt {
            json,
            trend,
            since,
            branch,
            badge,
            estimate,
        } => {
            let opts = debt::DebtOptions {
                json,
                trend,
                since,
                branch,
                badge,
                estimate,
                config_path: cli.global.config.clone(),
            };
            debt::execute_debt(&opts)?
        }
        Command::Watch {
            debounce,
            git_only,
            filter,
        } => {
            let project_root = std::env::current_dir()?;
            let project_root = index::resolve_project_root(&project_root).unwrap_or(project_root);
            let config_path = cli.global.config.as_deref();
            commands::watch::run_watch(
                &project_root,
                config_path,
                debounce,
                git_only,
                filter.as_deref(),
                cli.global.verbose,
            )?;
            0
        }
        Command::Install {
            list,
            agents,
            dry_run,
            force,
            yes,
            remove,
            validate,
        } => {
            let opts = commands::install::InstallOptions {
                list,
                agents,
                dry_run,
                force,
                yes,
                remove,
                validate,
            };
            let output = commands::install::execute_install(&opts)?;
            println!("{output}");
            0
        }
        Command::DeadCode {
            include_tests,
            include_pub,
            min_lines,
            json,
        } => {
            // Resolve the project root the same way `cora index` does, so
            // dead-code queries the workspace the index actually built (#522).
            let cwd = std::env::current_dir().with_context(|| "failed to get cwd")?;
            let project_root = index::resolve_project_root(&cwd).unwrap_or(cwd.clone());
            let conn = index::open_global_index()?;
            let project_id = index::ensure_project(&conn, &project_root)?;

            // Load config for entry_point_patterns
            let config = crate::config::loader::load_config(
                cli.global.config.as_deref(),
                None,
                None,
                None,
                None,
                false,
            )
            .unwrap_or_default();
            let entry_point_patterns = config.analysis.entry_point_patterns.clone();

            let opts = index::graph::DeadCodeOptions {
                include_tests,
                min_lines,
                entry_point_patterns,
                include_pub_api: include_pub,
            };
            let results = index::graph::find_dead_code(&conn, project_id, &opts)?;
            if json {
                let out = serde_json::to_string_pretty(&results)?;
                println!("{out}");
            } else if results.is_empty() {
                println!("✅ No dead code found.");
            } else {
                println!("☠ Found {} potentially dead symbol(s):\n", results.len());
                for r in &results {
                    println!(
                        "  {} {}:{} — {} {}",
                        r.name.dimmed(),
                        r.file.cyan(),
                        r.line.to_string().dimmed(),
                        r.kind.yellow(),
                        format!("({})", r.reason).dimmed()
                    );
                }
            }
            0
        }
        Command::Query { query, limit, json } => {
            let output = commands::query::execute_query_cli(&query, json, limit)?;
            println!("{output}");
            0
        }
        Command::Routes {
            method,
            prefix,
            json,
        } => {
            let output =
                commands::routes::execute_routes_cli(method.as_deref(), prefix.as_deref(), json)?;
            println!("{output}");
            0
        }
        Command::Serve => {
            commands::serve::execute_serve()?;
            0
        }
        Command::Upgrade { yes, check } => commands::upgrade::run(yes, check).await?,
    };

    std::process::exit(exit_code);
}

/// Struct to hold review options from CLI.
#[allow(clippy::struct_excessive_bools)]
struct ReviewOpts {
    staged: bool,
    unpushed: bool,
    base: Option<String>,
    commit: Option<String>,
    diff_file: Option<String>,
    unstaged: bool,
    max_diff_size: Option<usize>,
    stream: bool,
    no_auto_chunk: bool,
    progress: bool,
    quiet: bool,
    output_file: Option<String>,
    severity: Option<String>,
    upload: bool,
    repo: Option<String>,
    ref_name: Option<String>,
    token: Option<String>,
    no_cache: bool,
    ci: bool,
    memory: bool,
    learn: bool,
}

/// Struct to hold scan options from CLI.
struct ScanOpts {
    path: Option<String>,
    include: Vec<String>,
    exclude: Vec<String>,
    extensions: Vec<String>,
    incremental: bool,
    focus: Vec<String>,
    batch_files: usize,
    continue_on_batch_error: bool,
}

/// Handle the `review` subcommand.
async fn cmd_review(globals: &GlobalOptions, opts: ReviewOpts) -> Result<i32> {
    let config = loader::load_config(
        globals.config.as_deref(),
        globals.provider.as_deref(),
        globals.model.as_deref(),
        globals.base_url.as_deref(),
        globals.format.as_deref(),
        globals.no_color,
    )?;
    let llm_config = loader::build_llm_config(&config, globals.api_key.as_deref())?;

    // If --upload is set, force SARIF format
    let effective_format = if opts.upload {
        OutputFormat::Sarif
    } else {
        resolve_format(globals.format.as_deref(), &config)?
    };

    let progress_reporter = if opts.progress {
        progress::ProgressReporter::new()
    } else {
        progress::ProgressReporter::disabled()
    };

    // Emit started event if progress enabled
    progress_reporter.started("review", opts.base.as_deref());

    let review_opts = review::ReviewOptions {
        staged: opts.staged,
        unpushed: opts.unpushed,
        base: opts.base.clone(),
        commit: opts.commit.clone(),
        diff_file: opts.diff_file.clone(),
        unstaged: opts.unstaged,
        max_diff_size: opts.max_diff_size,
        stream: opts.stream,
        quiet: opts.quiet || opts.progress,
        severity: opts.severity.clone(),
        no_cache: opts.no_cache,
        ci: opts.ci,
        auto_chunk: !opts.no_auto_chunk,
    };

    // When streaming and not quiet/progress, show a simpler message
    if opts.stream && !opts.quiet && !opts.progress {
        eprintln!(
            "{}",
            format!(
                "⏳ Streaming review from {} ({})…",
                llm_config.provider, llm_config.model
            )
            .dimmed()
        );
    }

    // Uteke memory integration: recall context before review
    let mut memory_backend = memory::MemoryBackend::default();
    let memory_level = if opts.learn {
        memory::MemoryLevel::Learning
    } else if opts.memory {
        memory::MemoryLevel::Context
    } else {
        memory::MemoryLevel::None
    };

    let memory_context = if memory_level != memory::MemoryLevel::None {
        memory_backend.detect();
        if memory_backend.is_available() {
            let project = repo_name_from_git().unwrap_or_else(|| "unknown".to_string());
            let memories = memory_backend.recall_context(&project);
            memory_backend.build_memory_context(&memories)
        } else {
            if !opts.quiet {
                eprintln!(
                    "{}",
                    "⚠ --memory flag set but uteke not found on PATH. Continuing without memory."
                        .yellow()
                );
            }
            String::new()
        }
    } else {
        String::new()
    };

    // Execute the review (returns formatted output)
    let result = review::execute_review(
        &config,
        &llm_config,
        &review_opts,
        effective_format,
        &progress_reporter,
        if !memory_context.is_empty() {
            Some(memory_context.as_str())
        } else {
            None
        },
    )
    .await?;

    // Show memory status if context was used
    if !memory_context.is_empty() && !opts.quiet {
        eprintln!("{}", "ℹ Review enriched with Uteke memory context.".cyan());
    }

    // Print the formatted output (to file if --output-file, else stdout)
    if let Some(ref path) = opts.output_file {
        std::fs::write(path, &result.output)
            .with_context(|| format!("failed to write output file: {path}"))?;
        if !opts.quiet {
            eprintln!("{}", format!("Output written to {path}").dimmed());
        }
    } else {
        print!("{}", result.output);
    }

    // If --upload, send the SARIF output to GitHub Code Scanning
    if opts.upload {
        let sarif_content = result.output;
        upload_sarif_content(&sarif_content, &opts.repo, &opts.ref_name, &opts.token).await?;
    }

    // Uteke memory integration: save findings after review
    if memory_level == memory::MemoryLevel::Learning && memory_backend.is_available() {
        let project = repo_name_from_git().unwrap_or_else(|| "unknown".to_string());
        memory_backend.save_findings(
            &project,
            result.total_issues,
            &result.severity_summary,
            &result.categories,
        );
        if !opts.quiet {
            eprintln!(
                "{}",
                format!("💾 Saved {} findings to Uteke memory.", result.total_issues).dimmed()
            );
        }
    }

    Ok(result.exit_code)
}

/// Extract repo name from git remote origin URL.
fn repo_name_from_git() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Extract owner/repo from https://github.com/owner/repo.git
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() >= 2 {
        let repo = parts.last()?.trim_end_matches(".git").to_string();
        let owner = parts.get(parts.len() - 2)?;
        Some(format!("{}/{}", owner, repo))
    } else {
        None
    }
}

/// Upload a SARIF string to GitHub Code Scanning.
#[allow(clippy::ref_option)]
async fn upload_sarif_content(
    sarif_content: &str,
    repo: &Option<String>,
    ref_name: &Option<String>,
    token: &Option<String>,
) -> Result<i32> {
    use std::io::Write;

    // Write SARIF to a temp file and upload it
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!("cora-sarif-upload-{}.json", std::process::id()));

    {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(sarif_content.as_bytes())?;
    }

    println!(
        "{}",
        "\n→ Uploading SARIF to GitHub Code Scanning...".cyan()
    );

    let upload_opts = upload::UploadOptions {
        file: Some(tmp_path.to_string_lossy().to_string()),
        repo: repo.clone(),
        ref_name: ref_name.clone(),
        token: token.clone(),
    };

    let exit = upload::execute_upload(&upload_opts).await?;

    // Clean up temp file
    let _ = std::fs::remove_file(&tmp_path);

    Ok(exit)
}

/// Handle the `scan` subcommand.
async fn cmd_scan(globals: &GlobalOptions, opts: ScanOpts) -> Result<i32> {
    let config = loader::load_config(
        globals.config.as_deref(),
        globals.provider.as_deref(),
        globals.model.as_deref(),
        globals.base_url.as_deref(),
        globals.format.as_deref(),
        globals.no_color,
    )?;
    let llm_config = loader::build_llm_config(&config, globals.api_key.as_deref())?;

    let format = resolve_format(globals.format.as_deref(), &config)?;

    let scan_opts = scan::ScanOptions {
        path: opts.path,
        include: opts.include,
        exclude: opts.exclude,
        extensions: opts.extensions,
        incremental: opts.incremental,
        focus: opts.focus,
        batch_files: opts.batch_files,
        continue_on_batch_error: opts.continue_on_batch_error,
    };

    scan::execute_scan(&config, &llm_config, &scan_opts, format).await
}

/// Resolve the output format: CLI flag > config > default.
fn resolve_format(
    cli_format: Option<&str>,
    config: &crate::config::schema::Config,
) -> Result<OutputFormat> {
    let fmt_str = cli_format.map_or_else(
        || config.output.format.clone(),
        std::string::ToString::to_string,
    );
    OutputFormat::from_str_loose(&fmt_str)
}
