use std::io::{self, Write};

use anyhow::Result;
use colored::Colorize;

use crate::config::loader;
use crate::config::providers::PRESETS;

/// Execute `auth login` — interactive or non-interactive provider selection and API key setup.
///
/// When `--provider` flag is provided, runs non-interactively (scriptable).
/// Otherwise falls back to interactive mode.
pub fn execute_auth_login(
    cli_provider: Option<&str>,
    cli_api_key: Option<&str>,
    cli_model: Option<&str>,
    cli_base_url: Option<&str>,
    force: bool,
) -> Result<()> {
    // Non-interactive mode: provider flag provided
    if let Some(provider) = cli_provider {
        return execute_auth_login_noninteractive(
            provider,
            cli_api_key,
            cli_model,
            cli_base_url,
            force,
        );
    }

    // Interactive mode
    execute_auth_login_interactive()
}

/// Non-interactive auth login — used when --provider flag is provided.
///
/// API key resolution:
///   --api-key flag → provider env var (e.g. ZAI_API_KEY) → error
fn execute_auth_login_noninteractive(
    provider: &str,
    cli_api_key: Option<&str>,
    cli_model: Option<&str>,
    cli_base_url: Option<&str>,
    force: bool,
) -> Result<()> {
    // Check if already logged in
    if !force {
        let status = loader::auth_status()?;
        if status.has_key {
            eprintln!(
                "{}",
                "⚠️  An API key is already configured.".yellow().bold()
            );
            eprintln!("   Source: {}", status.source);
            eprintln!("   Use --force to overwrite.");
            anyhow::bail!("Aborted: key already exists. Use --force to overwrite.");
        }
    }

    // Resolve API key: --api-key flag → provider env var
    let api_key = if let Some(key) = cli_api_key {
        if key.is_empty() {
            anyhow::bail!("API key cannot be empty");
        }
        key.to_string()
    } else {
        // Try to auto-detect from provider-specific env var
        if let Some(preset) = PRESETS.iter().find(|p| p.name == provider) {
            if let Ok(key) = std::env::var(preset.env_key) {
                eprintln!(
                    "   {} Using {} from environment",
                    "→".green(),
                    preset.env_key.green()
                );
                key
            } else {
                anyhow::bail!(
                    "No API key provided. Set --api-key or export {}",
                    preset.env_key
                );
            }
        } else {
            anyhow::bail!(
                "No API key provided. Use --api-key for custom provider '{}'",
                provider
            );
        }
    };

    // Resolve preset defaults for the provider
    let (base_url, model) = if let Some(preset) = PRESETS.iter().find(|p| p.name == provider) {
        (
            cli_base_url.unwrap_or(preset.default_base_url).to_string(),
            cli_model.unwrap_or(preset.default_model).to_string(),
        )
    } else {
        // Custom provider — model and base_url required
        let base_url = cli_base_url
            .ok_or_else(|| anyhow::anyhow!("--base-url is required for custom provider '{}'. Use a known provider or provide all flags.", provider))?;
        let model = cli_model
            .ok_or_else(|| anyhow::anyhow!("--model is required for custom provider '{}'. Use a known provider or provide all flags.", provider))?;
        (base_url.to_string(), model.to_string())
    };

    // Save
    loader::save_api_key(&api_key)?;
    loader::save_provider_info(provider, &base_url, &model)?;

    println!(
        "{} API key saved to {}",
        "✅".green().bold(),
        "~/.codecora/cora-code/auth.toml".green()
    );
    println!(
        "{} Provider: {} | Model: {} | Base: {}",
        "   ".dimmed(),
        provider.bold(),
        model.bold(),
        base_url.dimmed()
    );

    Ok(())
}

/// Interactive auth login — guided setup with suggestions and auto-detection.
fn execute_auth_login_interactive() -> Result<()> {
    // Check if already logged in
    let status = loader::auth_status()?;
    if status.has_key {
        println!(
            "{}",
            "⚠️  An API key is already configured.".yellow().bold()
        );
        println!("   Source: {}", status.source);
        print!("   Overwrite? [y/N] ");
        io::stdout().flush()?;

        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        if !response.trim().eq_ignore_ascii_case("y") {
            println!("   Aborted.");
            return Ok(());
        }
    }

    println!();
    println!("{}", "🔑 Cora Auth Setup".bold());
    println!("{}", "   Choose your LLM provider:".dimmed());
    println!();

    // List known providers
    for (i, preset) in PRESETS.iter().enumerate() {
        println!(
            "  {} {}",
            format!("[{}]", i + 1).cyan().bold(),
            preset.name.bold(),
        );
    }
    // Custom option
    println!(
        "  {} {} — use any OpenAI-compatible endpoint",
        format!("[{}]", PRESETS.len() + 1).cyan().bold(),
        "custom".bold(),
    );
    println!();

    print!(
        "  {} ",
        format!("Select provider [1-{}]:", PRESETS.len() + 1).bold()
    );
    io::stdout().flush()?;

    let mut choice = String::new();
    io::stdin().read_line(&mut choice)?;
    let choice_num: usize = choice
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid choice — enter a number"))?;

    if choice_num == 0 || choice_num > PRESETS.len() + 1 {
        anyhow::bail!(
            "Invalid choice — pick a number between 1 and {}",
            PRESETS.len() + 1
        );
    }

    let (provider, default_base_url, default_model) = if choice_num <= PRESETS.len() {
        let preset = &PRESETS[choice_num - 1];
        (
            preset.name.to_string(),
            preset.default_base_url.to_string(),
            preset.default_model.to_string(),
        )
    } else {
        // Custom provider — collect all details
        println!();
        println!("  {} {}", "→".green(), "Custom provider setup".bold());
        println!();

        let provider = prompt_input("  Provider name (e.g. my-llm):")?;
        let base_url = prompt_input("  Base URL (e.g. https://api.example.com/v1):")?;
        let model = prompt_input("  Default model (e.g. gpt-4o):")?;

        if provider.is_empty() || base_url.is_empty() || model.is_empty() {
            anyhow::bail!("Provider name, base URL, and model are required for custom providers");
        }

        // Skip further prompts — custom provider already has everything
        println!();
        print!("  🔑 Enter your API key: ");
        io::stdout().flush()?;

        let mut key = String::new();
        io::stdin().read_line(&mut key)?;
        let key = key.trim().to_string();

        if key.is_empty() {
            anyhow::bail!("API key cannot be empty");
        }

        loader::save_api_key(&key)?;
        loader::save_provider_info(&provider, &base_url, &model)?;

        println!();
        print_saved(&provider, &model, &base_url);
        return Ok(());
    };

    // Known provider flow
    println!();
    println!(
        "  {} {} ({})",
        "→".green(),
        "Provider:".bold(),
        provider.green()
    );

    // --- API Key: auto-detect from provider env var ---
    let env_key = PRESETS
        .iter()
        .find(|p| p.name == provider)
        .map(|p| p.env_key)
        .unwrap_or("");

    let api_key = if let Ok(key) = std::env::var(env_key) {
        println!("  {} Found {} in environment", "→".green(), env_key.green());
        print!("  {} Use it? [Y/n]: ", "🔑".to_string().bold());
        io::stdout().flush()?;

        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().is_empty() || answer.trim().eq_ignore_ascii_case("y") {
            println!(
                "  {} Using {} from environment",
                "✅".green(),
                env_key.green()
            );
            key
        } else {
            prompt_secret("  🔑 Enter your API key:")?
        }
    } else {
        prompt_secret("  🔑 Enter your API key:")?
    };

    if api_key.is_empty() {
        anyhow::bail!("API key cannot be empty");
    }

    // --- Model: suggest default, allow override ---
    let model = prompt_with_default("  Model", &default_model)?;

    // --- Base URL: suggest default, allow override ---
    let base_url = prompt_with_default("  Base URL", &default_base_url)?;

    // Save
    loader::save_api_key(&api_key)?;
    loader::save_provider_info(&provider, &base_url, &model)?;

    println!();
    print_saved(&provider, &model, &base_url);
    println!(
        "{}",
        "   This file is local to your machine and not committed to git.".dimmed()
    );
    println!();
    println!(
        "{} Run {} to verify, or {} to start reviewing.",
        "Next:".bold(),
        "cora auth status".cyan(),
        "cora review".cyan()
    );

    Ok(())
}

/// Prompt for a non-empty string with label.
fn prompt_input(label: &str) -> Result<String> {
    print!("{} ", label);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Prompt for a secret value (API key) — reads line without echoing.
/// Falls back to normal readline if terminal control unavailable.
fn prompt_secret(prompt: &str) -> Result<String> {
    print!("{} ", prompt);
    io::stdout().flush()?;

    // Fallback: normal readline (no hidden echo — user can use --api-key for secrets)
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Prompt with a default value. Enter = accept default.
fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    print!("  {} [{}]: ", label.bold(), default.dimmed());
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_string();

    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed
    })
}

/// Print the saved confirmation message.
fn print_saved(provider: &str, model: &str, base_url: &str) {
    println!(
        "{} API key saved to {}",
        "✅".green().bold(),
        "~/.codecora/cora-code/auth.toml".green()
    );
    println!(
        "{} Provider: {} | Model: {} | Base: {}",
        "   ".dimmed(),
        provider.bold(),
        model.bold(),
        base_url.dimmed()
    );
}

/// Execute `auth status` — show whether an API key is configured and which provider.
pub fn execute_auth_status() -> Result<()> {
    let status = loader::auth_status()?;

    if status.has_key {
        println!("{} API key is configured.", "✅".green().bold());
        println!("   Source: {}", status.source);

        // Show stored provider info if available
        if let Some(info) = loader::load_provider_info()? {
            println!();
            println!("   {} {}", "Provider:".bold(), info.provider.green());
            println!("   {} {}", "Model:".bold(), info.model.green());
            println!("   {} {}", "Base URL:".bold(), info.base_url.dimmed());
        } else {
            println!(
                "   {} No provider info stored — run {} to set up",
                "ℹ️".yellow(),
                "cora auth login".cyan()
            );
        }
    } else {
        println!("{} No API key configured.", "❌".red().bold());
        println!("   Set it via:");
        println!("     • {} (interactive setup)", "cora auth login".cyan());
        println!(
            "     • {} (non-interactive)",
            "cora auth login --provider zai".cyan()
        );
        println!(
            "     • Provider-specific env vars will be auto-detected (e.g. ZAI_API_KEY, OPENAI_API_KEY)"
        );
    }

    Ok(())
}

/// Execute `auth remove` — delete the stored API key and provider info.
pub fn execute_auth_remove() -> Result<()> {
    let status = loader::auth_status()?;
    if !status.has_key {
        println!("{}", "No API key found to remove.".yellow());
        return Ok(());
    }

    loader::remove_api_key()?;
    loader::remove_provider_info()?;
    println!(
        "{} API key and provider info removed from local config.",
        "✅".green().bold()
    );

    Ok(())
}
