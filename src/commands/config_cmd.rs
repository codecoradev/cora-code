use std::path::PathBuf;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::loader;
use crate::config::schema::{CoraFile, HookSection, OutputSection, ProviderSection};

/// Execute `cora config show` — print the current resolved configuration.
///
/// `--global` shows only ~/.cora/config.yaml
/// `--project` shows only .cora.yaml
/// (default) shows the fully merged effective config
pub fn execute_config_show(
    global_only: bool,
    project_only: bool,
    config_path: Option<&str>,
) -> Result<()> {
    // Clap conflicts_with handles the --global + --project case,
    // but add a defensive check in case of programmatic invocation.
    if global_only {
        return show_global_config();
    }
    if project_only {
        return show_project_config();
    }
    show_effective_config(config_path)
}

/// Show the fully merged effective config (default behavior).
fn show_effective_config(config_path: Option<&str>) -> Result<()> {
    let config = loader::load_config(config_path, None, None, None, None, false)?;

    // Resolve effective values (env vars can override config file)
    let eff_provider = std::env::var("CORA_PROVIDER")
        .ok()
        .unwrap_or_else(|| config.provider.provider.clone());
    let eff_model = std::env::var("CORA_MODEL")
        .ok()
        .unwrap_or_else(|| config.provider.model.clone());
    let eff_base_url = std::env::var("CORA_BASE_URL")
        .ok()
        .unwrap_or_else(|| config.provider.base_url.clone());

    // Source annotations
    let provider_src = if std::env::var("CORA_PROVIDER").is_ok() {
        " [from: env CORA_PROVIDER]"
    } else {
        ""
    };
    let model_src = if std::env::var("CORA_MODEL").is_ok() {
        " [from: env CORA_MODEL]"
    } else {
        ""
    };
    let url_src = if std::env::var("CORA_BASE_URL").is_ok() {
        " [from: env CORA_BASE_URL]"
    } else {
        ""
    };

    println!(
        "{}",
        "╔══════════════════════════════════════════╗".cyan().bold()
    );
    println!(
        "{}",
        "║       Effective Configuration             ║"
            .cyan()
            .bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════════════╝".cyan().bold()
    );
    println!();

    println!(
        "{} {}{}",
        "provider:".bold(),
        eff_provider.green(),
        provider_src.dimmed()
    );
    println!(
        "  {} {}{}",
        "model:".dimmed(),
        eff_model.green(),
        model_src.dimmed()
    );
    println!(
        "  {} {}{}",
        "base_url:".dimmed(),
        eff_base_url.green(),
        url_src.dimmed()
    );

    println!(
        "{} {}",
        "focus:".bold(),
        config
            .focus
            .iter()
            .map(|f| format!("{}", f.green()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if !config.rules.is_empty() {
        println!(
            "{} {}",
            "rules:".bold(),
            config
                .rules
                .iter()
                .map(|r| format!("{}", r.green()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    println!(
        "{} mode={} min_severity={} max_diff_size={}",
        "hook:".bold(),
        config.hook.mode.yellow(),
        config.hook.min_severity.yellow(),
        config.hook.max_diff_size
    );

    println!(
        "{} format={} color={}",
        "output:".bold(),
        config.output.format.green(),
        config.output.color
    );

    println!(
        "{} {}",
        "ignore files:".bold(),
        if config.ignore.files.is_empty() {
            "(none)".to_string().dimmed().to_string()
        } else {
            config
                .ignore
                .files
                .iter()
                .map(|f| format!("{}", f.dimmed()))
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    Ok(())
}

/// Show only the global config (~/.cora/config.yaml).
fn show_global_config() -> Result<()> {
    let dir = loader::cora_dir()?;
    let path = dir.join("config.yaml");

    println!(
        "{}",
        "╔══════════════════════════════════════════╗".cyan().bold()
    );
    println!(
        "{}",
        "║         Global Configuration             ║".cyan().bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════════════╝".cyan().bold()
    );
    println!();

    if !path.is_file() {
        println!(
            "{} No global config found at {}",
            "⚠️".yellow(),
            path.display()
        );
        println!(
            "   Run {} or {} to create one.",
            "cora auth login".cyan(),
            "cora config set --global provider zai".cyan()
        );
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let cora = CoraFile::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    println!("{} {}", "File:".bold(), path.display());
    println!();
    print_cora_file(&cora);

    // Also show auth status
    println!();
    let auth = loader::auth_status()?;
    if auth.has_key {
        println!(
            "{} API key: configured ({})",
            "✅".green().bold(),
            auth.source
        );
    } else {
        println!("{} API key: not configured", "❌".red().bold());
    }

    Ok(())
}

/// Show only the project config (.cora.yaml).
fn show_project_config() -> Result<()> {
    println!(
        "{}",
        "╔══════════════════════════════════════════╗".cyan().bold()
    );
    println!(
        "{}",
        "║        Project Configuration             ║".cyan().bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════════════╝".cyan().bold()
    );
    println!();

    let found = loader::find_cora_file(&std::env::current_dir()?)?;
    match found {
        Some((path, cora)) => {
            println!("{} {}", "File:".bold(), path.display());
            println!();
            print_cora_file(&cora);
        }
        None => {
            println!("{} No .cora.yaml found in this project.", "⚠️".yellow());
            println!("   Run {} to create one.", "cora init".cyan());
        }
    }

    Ok(())
}

/// Print a CoraFile's non-empty fields.
fn print_cora_file(cora: &CoraFile) {
    if let Some(ps) = &cora.provider {
        if let Some(v) = &ps.provider {
            println!("{} {}", "provider:".bold(), v.green());
        }
        if let Some(v) = &ps.model {
            println!("  {} {}", "model:".dimmed(), v.green());
        }
        if let Some(v) = &ps.base_url {
            println!("  {} {}", "base_url:".dimmed(), v.dimmed());
        }
    }
    if let Some(v) = &cora.model {
        println!("{} {}", "model:".bold(), v.green());
    }
    if let Some(v) = &cora.base_url {
        println!("{} {}", "base_url:".bold(), v.dimmed());
    }
    if let Some(v) = &cora.focus {
        println!(
            "{} {}",
            "focus:".bold(),
            v.iter()
                .map(|f| format!("{}", f.green()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(v) = &cora.rules {
        if !v.is_empty() {
            println!(
                "{} {}",
                "rules:".bold(),
                v.iter()
                    .map(|r| format!("{}", r.green()))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if let Some(ig) = &cora.ignore {
        if let Some(v) = &ig.files {
            println!(
                "{} {}",
                "ignore files:".bold(),
                v.iter()
                    .map(|f| format!("{}", f.dimmed()))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if let Some(h) = &cora.hook {
        let mut parts = vec![];
        if let Some(v) = &h.mode {
            parts.push(format!("mode={}", v));
        }
        if let Some(v) = &h.min_severity {
            parts.push(format!("min_severity={}", v));
        }
        if let Some(v) = h.max_diff_size {
            parts.push(format!("max_diff_size={}", v));
        }
        if !parts.is_empty() {
            println!("{} {}", "hook:".bold(), parts.join(" ").yellow());
        }
    }
    if let Some(o) = &cora.output {
        let mut parts = vec![];
        if let Some(v) = &o.format {
            parts.push(format!("format={}", v));
        }
        if let Some(v) = o.color {
            parts.push(format!("color={}", v));
        }
        if !parts.is_empty() {
            println!("{} {}", "output:".bold(), parts.join(" ").green());
        }
    }
    if let Some(llm) = &cora.llm {
        let mut parts = vec![];
        if let Some(v) = llm.temperature {
            parts.push(format!("temperature={}", v));
        }
        if let Some(v) = llm.max_tokens {
            parts.push(format!("max_tokens={}", v));
        }
        if let Some(ref v) = llm.max_tokens_param {
            parts.push(format!("max_tokens_param={}", v));
        }
        if let Some(v) = llm.timeout {
            parts.push(format!("timeout={}", v));
        }
        if let Some(v) = llm.cache_ttl {
            parts.push(format!("cache_ttl={}", v));
        }
        if !parts.is_empty() {
            println!("{} {}", "llm:".bold(), parts.join(" ").yellow());
        }
    }
}
/// to a YAML config file.
///
/// Supported keys: model, provider, `base_url`, format, severity
pub fn execute_config_set(key: &str, value: &str, global: bool) -> Result<()> {
    // Validate the key
    match key {
        "model" | "provider" | "base_url" | "format" | "severity" => {}
        _ => {
            anyhow::bail!(
                "unsupported key: {key}\nSupported keys: model, provider, base_url, format, severity"
            );
        }
    }

    // Determine target path
    let path = if global {
        let dir = loader::cora_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        dir.join("config.yaml")
    } else {
        // Warn if not inside a git project (could create orphan file)
        if std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            PathBuf::from(".cora.yaml")
        } else {
            anyhow::bail!(
                "Not in a git project. Use --global to set config in ~/.cora/config.yaml, or cd into a git repo first."
            );
        }
    };

    // Load existing file or start fresh
    let mut cora = if path.is_file() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        CoraFile::from_str(&content).unwrap_or_default()
    } else {
        CoraFile::default()
    };

    // Apply the key update
    match key {
        "model" => {
            let provider = cora.provider.get_or_insert_with(ProviderSection::default);
            provider.model = Some(value.to_string());
        }
        "provider" => {
            let provider = cora.provider.get_or_insert_with(ProviderSection::default);
            provider.provider = Some(value.to_string());
        }
        "base_url" => {
            let provider = cora.provider.get_or_insert_with(ProviderSection::default);
            provider.base_url = Some(value.to_string());
        }
        "format" => {
            let output = cora.output.get_or_insert_with(OutputSection::default);
            output.format = Some(value.to_string());
        }
        "severity" => {
            let hook = cora.hook.get_or_insert_with(HookSection::default);
            hook.min_severity = Some(value.to_string());
        }
        _ => unreachable!(),
    }

    // Write back as YAML
    let yaml = serde_yaml_ng::to_string(&cora).context("failed to serialize config to YAML")?;
    std::fs::write(&path, &yaml).with_context(|| format!("failed to write {}", path.display()))?;

    // Restrict permissions for global config (defense-in-depth)
    #[cfg(unix)]
    if global {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let scope = if global { "global" } else { "project" };
    println!(
        "{} Set {} = {} in {} ({})",
        "✓".green().bold(),
        key.bold(),
        value.green(),
        path.display(),
        scope.dimmed()
    );

    Ok(())
}

/// Execute `cora config validate` — load config and report validity.
///
/// Returns exit code: 0 if valid, 2 if issues found.
pub fn execute_config_validate(config_path: Option<&str>) -> Result<i32> {
    // 1. Load resolved config (same as other commands)
    let config = loader::load_config(config_path, None, None, None, None, false)?;

    // 2. Find the raw config file to check which fields were explicitly set.
    //    Prefer --config path, then fall back to auto-discovery.
    let cora_file = if let Some(path) = config_path {
        std::fs::read_to_string(path).ok().and_then(|content| {
            CoraFile::from_str(&content)
                .ok()
                .map(|cf| (std::path::PathBuf::from(path), cf))
        })
    } else {
        loader::find_cora_file(&std::env::current_dir().unwrap_or_default()).unwrap_or_else(|e| {
            eprintln!(
                "{} Warning: could not search for config file: {}",
                "⚠️".yellow(),
                e
            );
            None
        })
    };

    // Also check global config for fields set there
    let global_cora = load_global_cora_file();

    // Track how many fields were explicitly set
    let mut explicitly_set = 0u32;
    let total_fields = 8u32;

    // ── Check config file ──
    match &cora_file {
        Some((path, _)) => {
            println!("{} Config file: {}", "✅".green().bold(), path.display());
            explicitly_set += 1;
        }
        None => {
            println!(
                "{} Config file: not found (using defaults)",
                "⚠️".yellow().bold()
            );
        }
    }

    // ── Check provider ──
    let provider_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.provider.as_ref(),
        |g| g.provider.as_ref(),
    );
    if provider_set {
        println!(
            "{} Provider: {}",
            "✅".green().bold(),
            config.provider.provider
        );
        explicitly_set += 1;
    } else {
        println!(
            "{} Provider: not set (default: {})",
            "⚠️".yellow().bold(),
            config.provider.provider
        );
    }

    // ── Check model ──
    let model_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.provider.as_ref().and_then(|p| p.model.as_ref()),
        |g| g.provider.as_ref().and_then(|p| p.model.as_ref()),
    );
    if model_set {
        println!("{} Model: {}", "✅".green().bold(), config.provider.model);
        explicitly_set += 1;
    } else {
        println!(
            "{} Model: not set (default: {})",
            "⚠️".yellow().bold(),
            config.provider.model
        );
    }

    // ── Check API key ──
    let auth = loader::auth_status().context("failed to check auth status")?;
    if auth.has_key {
        let source = if auth.source.contains("env") {
            "env var".to_string()
        } else {
            format!("auth file ({})", auth.source)
        };
        println!("{} API key: configured ({})", "✅".green().bold(), source);
        explicitly_set += 1;
    } else {
        println!("{} API key: not configured", "❌".red().bold());
    }

    // ── Check severity ──
    let severity_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.hook.as_ref().and_then(|h| h.min_severity.as_ref()),
        |g| g.hook.as_ref().and_then(|h| h.min_severity.as_ref()),
    );
    if severity_set {
        println!(
            "{} Severity: {}",
            "✅".green().bold(),
            config.hook.min_severity
        );
        explicitly_set += 1;
    } else {
        println!(
            "{} Severity: not set (default: {})",
            "⚠️".yellow().bold(),
            config.hook.min_severity
        );
    }

    // ── Check output format ──
    let format_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.output.as_ref().and_then(|o| o.format.as_ref()),
        |g| g.output.as_ref().and_then(|o| o.format.as_ref()),
    );
    if format_set {
        println!(
            "{} Output format: {}",
            "✅".green().bold(),
            config.output.format
        );
        explicitly_set += 1;
    } else {
        println!(
            "{} Output format: not set (default: {})",
            "⚠️".yellow().bold(),
            config.output.format
        );
    }

    // ── Check temperature ──
    let temp_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.llm.as_ref().and_then(|l| l.temperature.as_ref()),
        |g| g.llm.as_ref().and_then(|l| l.temperature.as_ref()),
    );
    if temp_set {
        println!(
            "{} Temperature: {}",
            "✅".green().bold(),
            config.temperature
        );
        explicitly_set += 1;
    } else {
        println!(
            "{} Temperature: not set (default: {})",
            "⚠️".yellow().bold(),
            config.temperature
        );
    }

    // ── Check cache TTL ──
    let cache_set = is_explicitly_set(
        &cora_file,
        &global_cora,
        |c| c.llm.as_ref().and_then(|l| l.cache_ttl.as_ref()),
        |g| g.llm.as_ref().and_then(|l| l.cache_ttl.as_ref()),
    );
    if cache_set {
        println!("{} Cache TTL: {}", "✅".green().bold(), config.cache_ttl);
        explicitly_set += 1;
    } else {
        println!(
            "{} Cache TTL: not set (default: {})",
            "⚠️".yellow().bold(),
            config.cache_ttl
        );
    }

    println!();

    // Determine overall validity
    let has_issues = !auth.has_key;
    if has_issues {
        println!(
            "{}",
            "Configuration issues found. Fix the ❌ items above."
                .red()
                .bold()
        );
        println!(
            "{}",
            format!("{}/{} fields explicitly set.", explicitly_set, total_fields).dimmed()
        );
        Ok(2)
    } else {
        println!("{}", "Configuration valid.".green().bold());
        println!(
            "{}",
            format!("{}/{} fields explicitly set.", explicitly_set, total_fields).dimmed()
        );
        Ok(0)
    }
}

/// Check if a field was explicitly set in either the project config or global config.
/// Uses closure extractors to check both config sources.
fn is_explicitly_set<T>(
    project: &Option<(PathBuf, CoraFile)>,
    global: &Option<CoraFile>,
    project_extractor: impl Fn(&CoraFile) -> Option<&T>,
    global_extractor: impl Fn(&CoraFile) -> Option<&T>,
) -> bool {
    let project_has = project
        .as_ref()
        .and_then(|(_, c)| project_extractor(c))
        .is_some();
    let global_has = global.as_ref().and_then(global_extractor).is_some();
    project_has || global_has
}

/// Load the global config file (same logic as loader::load_global_config but returns Option directly).
fn load_global_cora_file() -> Option<CoraFile> {
    let dir = loader::cora_dir().ok()?;
    let path = dir.join("config.yaml");
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    CoraFile::from_str(&content).ok()
}
