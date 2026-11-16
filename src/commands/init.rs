use anyhow::{Context, Result};
use colored::Colorize;
use tracing::debug;

use crate::hook;

/// Example .cora.yaml template content.
const CONFIG_TEMPLATE: &str = r#"# Cora Code Review Configuration
# See: https://github.com/codecoradev/cora-code

# Provider settings
provider:
  provider: openai
  model: gpt-4o-mini
  base_url: https://api.openai.com/v1

# Review focus areas: security, performance, bugs, best-practice, style
focus:
  - security
  - performance
  - bugs

# Custom rules / additional instructions for the reviewer
rules: []
# rules:
#   - "Always check for SQL injection vulnerabilities"
#   - "Ensure all public functions have error handling"

# Ignore patterns
# Dependencies (node_modules, vendor, ...), dist/target, minified files, source maps
# and lockfiles are ignored by default. Add your own generated or vendored paths
# here; `keep: [...]` turns single built-ins off and `defaults: false` all of them.
ignore:
  files: []
#   files:
#     - "src/generated/**"
  rules: []
#   rules:
#     - "style"  # ignore style issues

# Hook settings
hook:
  mode: warn          # "warn" (print but allow) or "block" (fail commit)
  min_severity: major # minimum severity to trigger hook action
  max_diff_size: 5242880 # max 5MB diff chars before refusing

# Output settings
output:
  format: pretty      # pretty, json, compact, sarif
  color: true
"#;

/// Execute the init command: create a `.cora.yaml` file and install pre-commit hook.
pub fn execute_init(skip_hook: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config_path = cwd.join(".cora.yaml");

    if config_path.is_file() {
        anyhow::bail!(
            ".cora.yaml already exists at {}. Use --force to overwrite.",
            config_path.display()
        );
    }

    std::fs::write(&config_path, CONFIG_TEMPLATE)
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    debug!(path = %config_path.display(), "created .cora.yaml");
    println!(
        "{} Created {} with example configuration.",
        "✅".green().bold(),
        config_path.display()
    );
    println!(
        "{}",
        "   Edit the file to customize your review settings.".dimmed()
    );
    println!(
        "{}",
        "   API keys should be set via `cora auth login`.".dimmed()
    );

    // Install pre-commit hook unless --no-hook is specified
    if !skip_hook {
        install_hook_silent();
    } else {
        println!(
            "{}",
            "   Skipped hook installation (--no-hook). Run `cora hook install` later.".dimmed()
        );
    }

    Ok(())
}

/// Execute the init command with --force flag.
pub fn execute_init_force(skip_hook: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config_path = cwd.join(".cora.yaml");

    std::fs::write(&config_path, CONFIG_TEMPLATE)
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    debug!(path = %config_path.display(), "overwrote .cora.yaml");
    println!(
        "{} Overwrote {} with example configuration.",
        "✅".green().bold(),
        config_path.display()
    );

    // Install pre-commit hook unless --no-hook is specified
    if !skip_hook {
        install_hook_silent();
    } else {
        println!(
            "{}",
            "   Skipped hook installation (--no-hook). Run `cora hook install` later.".dimmed()
        );
    }

    Ok(())
}

/// Install hook silently — warns on failure but doesn't fail init.
fn install_hook_silent() {
    match hook::install_hook() {
        Ok(path) => {
            println!(
                "{} Installed pre-commit hook to {}",
                "✅".green().bold(),
                path
            );
            println!(
                "{}",
                "   The hook will run `cora review --staged --format compact` before each commit."
                    .dimmed()
            );
        }
        Err(e) => {
            // Don't fail init if hook install fails (e.g. not a git repo)
            println!(
                "{} Could not install pre-commit hook: {}",
                "⚠️".yellow().bold(),
                e
            );
            println!(
                "{}",
                "   Run `cora hook install` later from inside a git repository.".dimmed()
            );
        }
    }
}
