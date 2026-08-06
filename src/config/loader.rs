use std::path::{Path, PathBuf};

use crate::error::CoraError;
use tracing::debug;

use crate::config::providers::{PRESETS, detected_presets};
use crate::config::schema::{Config, CoraFile};
use crate::engine::LLMConfig;

/// The name of the config file we search for in projects.
const CONFIG_FILENAME: &str = ".cora.yaml";

/// Name of the global config file.
const GLOBAL_CONFIG_FILENAME: &str = "config.yaml";

/// Name of the secret config file for API keys (never committed).
const AUTH_FILENAME: &str = "auth.toml";

/// Name of the old auth file (for migration).
const OLD_AUTH_FILENAME: &str = "config.toml";

/// Marker file created after successful migration.
const MIGRATION_MARKER: &str = ".migrated";

/// Locate the `.cora.yaml` config by walking parent directories from `start`.
/// Returns the path and parsed content, or `None` if not found.
pub fn find_cora_file(start: &Path) -> std::result::Result<Option<(PathBuf, CoraFile)>, CoraError> {
    let mut dir = if start.is_file() {
        start.parent().unwrap_or(start).to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = dir.join(CONFIG_FILENAME);
        if candidate.is_file() {
            debug!(path = %candidate.display(), "found .cora.yaml");
            let content = std::fs::read_to_string(&candidate)
                .map_err(|e| CoraError::ConfigRead(format!("{}: {}", candidate.display(), e)))?;
            let cora = CoraFile::from_str(&content).map_err(|e| {
                CoraError::ConfigParse(format!(
                    "{}\n  → file: {}\n  → hint: check for syntax errors (indentation, colons, trailing spaces)",
                    e,
                    candidate.display()
                ))
            })?;
            return Ok(Some((candidate, cora)));
        }

        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return Ok(None),
        }
    }
}

/// Load the global config from `~/.codecora/cora-code/config.yaml`.
/// Returns `None` if the file doesn't exist or can't be parsed.
fn load_global_config() -> std::result::Result<Option<CoraFile>, CoraError> {
    let dir = cora_dir()?;
    let path = dir.join(GLOBAL_CONFIG_FILENAME);
    if !path.is_file() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| CoraError::ConfigRead(format!("{}: {}", path.display(), e)))?;
    let cora = CoraFile::from_str(&content)?;
    debug!(path = %path.display(), "loaded global config");
    Ok(Some(cora))
}

/// Migrate old `config.toml` to the new format if it exists.
/// - Non-secret keys → `config.yaml`
/// - `api_key` → `auth.toml`
/// - Delete the old file after successful migration.
/// - Creates `.migrated` marker to prevent re-running.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn migrate_old_config() {
    let Ok(dir) = cora_dir() else {
        return;
    };
    let old_path = dir.join(OLD_AUTH_FILENAME);
    if !old_path.is_file() {
        return;
    }

    // Check if migration already completed
    if dir.join(MIGRATION_MARKER).is_file() {
        return;
    }

    let content = match std::fs::read_to_string(&old_path) {
        Ok(c) => c,
        Err(e) => {
            debug!("failed to read old config for migration: {}", e);
            return;
        }
    };

    let table: toml::Table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            debug!(
                "old config.toml is not valid TOML, skipping migration: {}",
                e
            );
            return;
        }
    };

    // Check if there's an api_key
    let api_key = table
        .get("auth")
        .and_then(|a| a.get("api_key"))
        .and_then(|k| k.as_str())
        .map(std::string::ToString::to_string)
        .or_else(|| {
            table
                .get("api_key")
                .and_then(|k| k.as_str())
                .map(std::string::ToString::to_string)
        });

    // Extract non-secret config fields into a CoraFile
    let mut cora = CoraFile::default();

    if let Some(provider) = table.get("provider").and_then(|v| v.as_table()) {
        let mut ps = crate::config::schema::ProviderSection::default();
        if let Some(v) = provider.get("provider").and_then(|v| v.as_str()) {
            ps.provider = Some(v.to_string());
        }
        if let Some(v) = provider.get("model").and_then(|v| v.as_str()) {
            ps.model = Some(v.to_string());
        }
        if let Some(v) = provider.get("base_url").and_then(|v| v.as_str()) {
            ps.base_url = Some(v.to_string());
        }
        // Only set if we found something
        if ps.provider.is_some() || ps.model.is_some() || ps.base_url.is_some() {
            cora.provider = Some(ps);
        }
    }

    if let Some(output) = table.get("output").and_then(|v| v.as_table()) {
        let mut os = crate::config::schema::OutputSection::default();
        if let Some(v) = output.get("format").and_then(|v| v.as_str()) {
            os.format = Some(v.to_string());
        }
        if let Some(v) = output.get("color").and_then(toml::Value::as_bool) {
            os.color = Some(v);
        }
        if os.format.is_some() || os.color.is_some() {
            cora.output = Some(os);
        }
    }

    if let Some(hook) = table.get("hook").and_then(|v| v.as_table()) {
        let mut hs = crate::config::schema::HookSection::default();
        if let Some(v) = hook.get("mode").and_then(|v| v.as_str()) {
            hs.mode = Some(v.to_string());
        }
        if let Some(v) = hook.get("min_severity").and_then(|v| v.as_str()) {
            hs.min_severity = Some(v.to_string());
        }
        if let Some(v) = hook.get("max_diff_size").and_then(toml::Value::as_integer) {
            hs.max_diff_size = Some(v as usize);
        }
        if hs.mode.is_some() || hs.min_severity.is_some() || hs.max_diff_size.is_some() {
            cora.hook = Some(hs);
        }
    }

    // Write api_key to auth.toml if present (use TOML library to avoid injection)
    if let Some(key) = api_key {
        let auth_path = dir.join(AUTH_FILENAME);
        let mut table = toml::Table::new();
        table.insert("api_key".to_string(), toml::Value::String(key));
        let content = table.to_string();
        if let Err(e) = std::fs::write(&auth_path, content) {
            debug!("failed to write migrated auth.toml: {}", e);
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&auth_path, perms);
        }
    }

    // Write config to config.yaml if there are non-secret fields
    let has_config = cora.provider.is_some()
        || cora.output.is_some()
        || cora.hook.is_some()
        || cora.focus.is_some()
        || cora.rules.is_some()
        || cora.ignore.is_some();

    if has_config {
        let config_path = dir.join(GLOBAL_CONFIG_FILENAME);
        if let Ok(yaml) = serde_yaml_ng::to_string(&cora) {
            if let Err(e) = std::fs::write(&config_path, yaml) {
                debug!("failed to write migrated config.yaml: {}", e);
                return;
            }
        }
    }

    // Delete old file and create migration marker
    if let Err(e) = std::fs::remove_file(&old_path) {
        debug!("failed to remove old config.toml after migration: {}", e);
    } else {
        // Create marker to prevent re-running migration
        let _ = std::fs::write(dir.join(MIGRATION_MARKER), "");
        debug!("migrated config.toml to new format");
    }
}

/// Resolve the max tokens parameter name based on provider and config setting.
///
/// - If `config_value` is not `"auto"`, use it directly (explicit override).
/// - Otherwise, detect from provider name:
///   - `gemini`, `google`, `vertex` → `"max_output_tokens"`
///   - Everything else → `"max_tokens"`
pub fn resolve_max_tokens_param(provider: &str, config_value: &str) -> String {
    match config_value {
        v if v != "auto" => v.to_string(),
        _ => match provider {
            "gemini" | "google" | "vertex" => "max_output_tokens".to_string(),
            _ => "max_tokens".to_string(),
        },
    }
}

/// Load the full resolved config: defaults ← global config ← .cora.yaml ← CLI overrides.
///
/// `cli_provider`, `cli_model`, `cli_api_key`, and `cli_format` are `None`
/// when the user did not pass the corresponding flag.
pub fn load_config(
    cli_config_path: Option<&str>,
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    cli_base_url: Option<&str>,
    cli_format: Option<&str>,
    no_color: bool,
) -> std::result::Result<Config, CoraError> {
    let mut config = Config::default();

    // Run migrations silently on first access
    migrate_old_config();
    migrate_provider_info_from_auth().unwrap_or_else(|e| {
        debug!("provider migration failed: {}", e);
    });

    // 1. Load global config (~/.codecora/cora-code/config.yaml)
    if let Some(cora) = load_global_config()? {
        cora.merge_into(&mut config)?;
    }

    // 2. Load project config (.cora.yaml)
    if let Some(path) = cli_config_path {
        let path = Path::new(path);
        let content = std::fs::read_to_string(path)
            .map_err(|e| CoraError::ConfigRead(format!("{}: {}", path.display(), e)))?;
        let cora = CoraFile::from_str(&content).map_err(|e| {
            CoraError::ConfigParse(format!(
                "{}\n  → file: {}\n  → hint: check for syntax errors (indentation, colons, trailing spaces)",
                e,
                path.display()
            ))
        })?;
        cora.merge_into(&mut config)?;
        debug!(path = %path.display(), "loaded explicit config");
    } else if let Some((path, cora)) = find_cora_file(&std::env::current_dir()?)? {
        cora.merge_into(&mut config)?;
        debug!(path = %path.display(), "loaded discovered config");
    } else {
        debug!("no .cora.yaml found, using defaults");
    }

    // 3. CLI overrides
    if let Some(p) = cli_provider {
        config.provider.provider = p.to_string();
    }
    if let Some(m) = cli_model {
        config.provider.model = m.to_string();
    }
    if let Some(u) = cli_base_url {
        config.provider.base_url = u.to_string();
    }
    if let Some(f) = cli_format {
        config.output.format = f.to_string();
    }
    if no_color {
        config.output.color = false;
    }

    // #94: validate semantic constraints after all sources are merged so typos
    // and out-of-range values fail loudly at load instead of at runtime.
    config.validate()?;

    Ok(config)
}

/// Build an `LLMConfig` from the resolved `Config`, fetching the API key
/// from: CLI flag / CORA_API_KEY env → ~/.codecora/cora-code/auth.toml → provider-specific env vars.
///
/// If none of those are set, auto-detect from known provider env vars (`OPENAI_API_KEY`, etc.)
/// and configure `provider/model/base_url` from the matching preset.
///
/// Provider/model/base_url resolution:
///   CORA_* env vars > .cora.yaml (project) > ~/.codecora/cora-code/config.yaml (global) > auto-detect > defaults
pub fn build_llm_config(
    config: &Config,
    cli_api_key: Option<&str>,
) -> std::result::Result<LLMConfig, CoraError> {
    // Resolve the API key: CLI flag / CORA_API_KEY → auth.toml → auto-detect
    let (api_key, auto_preset) = if let Some(key) = cli_api_key {
        (key.to_string(), None)
    } else if let Some(key) = load_api_key_from_auth_file()? {
        (key, None)
    } else {
        // No stored key — auto-detect from provider presets
        let detected = detected_presets();
        if detected.is_empty() {
            let _available: Vec<String> = PRESETS
                .iter()
                .map(|p| format!("  {} (set {})", p.name, p.env_key))
                .collect();
            return Err(CoraError::NoApiKey);
        }

        // Use the first detected provider
        let preset = detected[0];
        let key = std::env::var(preset.env_key).unwrap_or_default();

        if detected.len() > 1 {
            let names: Vec<&str> = detected.iter().map(|p| p.name).collect();
            eprintln!(
                "ℹ️  Multiple providers detected ({}). Using first: {}. Set CORA_PROVIDER or use --provider to override.",
                names.join(", "),
                preset.name
            );
        } else {
            debug!(provider = preset.name, "auto-detected provider from env");
        }

        (key, Some(preset))
    };

    // Resolve provider/model/base_url priority:
    //   CORA_* env vars > config (already merged: project > global > defaults) > auto-detected preset
    let cora_provider = std::env::var("CORA_PROVIDER").ok();
    let cora_model = std::env::var("CORA_MODEL").ok();
    let cora_base_url = std::env::var("CORA_BASE_URL").ok();

    let provider = cora_provider
        .or_else(|| {
            if config.provider.provider
                != crate::config::schema::Config::default().provider.provider
            {
                Some(config.provider.provider.clone())
            } else {
                auto_preset.map(|p| p.name.to_string())
            }
        })
        .unwrap_or_else(|| config.provider.provider.clone());

    let model = cora_model
        .or_else(|| {
            if config.provider.model != crate::config::schema::Config::default().provider.model {
                Some(config.provider.model.clone())
            } else {
                auto_preset.map(|p| p.default_model.to_string())
            }
        })
        .unwrap_or_else(|| config.provider.model.clone());

    let base_url = cora_base_url
        .or_else(|| {
            if config.provider.base_url
                != crate::config::schema::Config::default().provider.base_url
            {
                Some(config.provider.base_url.clone())
            } else {
                auto_preset
                    .and_then(|p| std::env::var(p.env_url).ok())
                    .or_else(|| auto_preset.map(|p| p.default_base_url.to_string()))
            }
        })
        .unwrap_or_else(|| config.provider.base_url.clone());

    let max_tokens_param = resolve_max_tokens_param(&provider, &config.max_tokens_param);

    Ok(LLMConfig {
        api_key,
        base_url,
        model,
        provider,
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        max_tokens_param,
        timeout: config.timeout,
    })
}

/// Get the cora config/data directory.
///
/// Returns `~/.codecora/cora-code/` (or `CODECORA_HOME` override).
/// On first call, migrates files from legacy `~/.cora/` if the new directory is empty.
///
/// ```text
/// Default:           $HOME/.codecora/cora-code/
/// CODECORA_HOME set: $CODECORA_HOME/cora-code/
/// ```
pub fn cora_dir() -> std::result::Result<PathBuf, CoraError> {
    let new_dir = crate::data_dir::cora_data_dir();

    // One-time migration from legacy ~/.cora/ → ~/.codecora/cora-code/
    migrate_legacy_cora_dir(&new_dir);

    Ok(new_dir)
}

/// Migrate files from legacy `~/.cora/` to the new `~/.codecora/cora-code/` directory.
///
/// Runs only once — a `.migrated` marker file prevents re-running.
/// Does NOT delete old files; users can clean up manually.
fn migrate_legacy_cora_dir(new_dir: &std::path::Path) {
    let marker = new_dir.join(".migrated-to-codecora");
    if marker.is_file() {
        return;
    }

    let Some(home) = dirs::home_dir() else {
        return;
    };
    let old_dir = home.join(".cora");
    if !old_dir.is_dir() {
        return;
    }

    // Ensure new directory exists
    if let Err(e) = std::fs::create_dir_all(new_dir) {
        debug!("skip migration, cannot create new dir: {e}");
        return;
    }

    // Files to migrate (copy, don't move — user can clean up old ones)
    let files_to_migrate = ["config.yaml", "auth.toml", "config.toml"];
    let mut migrated_any = false;

    for filename in &files_to_migrate {
        let old_path = old_dir.join(filename);
        let new_path = new_dir.join(filename);

        if old_path.is_file() && !new_path.exists() {
            match std::fs::copy(&old_path, &new_path) {
                Ok(_) => {
                    debug!("migrated {filename} from ~/.cora/ to ~/.codecora/cora-code/");
                    migrated_any = true;
                }
                Err(e) => {
                    debug!("failed to migrate {filename}: {e}");
                }
            }
        }
    }

    // Migrate old marker so we don't re-run the TOML→YAML migration
    let old_marker = old_dir.join(".migrated");
    if old_marker.is_file() {
        let new_marker = new_dir.join(".migrated");
        if !new_marker.exists() {
            let _ = std::fs::copy(&old_marker, &new_marker);
        }
    }

    // Write our marker so this migration never runs again
    let _ = std::fs::write(&marker, "1");

    if migrated_any {
        eprintln!(
            "ℹ️  Migrated cora config from ~/.cora/ to ~/.codecora/cora-code/. \
             Old files are kept as backup — safe to remove after verifying."
        );
    }
}

/// Read the stored API key from auth.toml.
pub fn load_api_key_from_auth_file() -> std::result::Result<Option<String>, CoraError> {
    let dir = cora_dir()?;
    let path = dir.join(AUTH_FILENAME);
    if !path.is_file() {
        return Ok(None);
    }

    // Security: warn if auth file has overly permissive permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&path) {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                // Auto-fix: restrict permissions to owner-only
                let fixed = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                if fixed.is_ok() {
                    debug!("auto-fixed auth file permissions: {:o} → 600", mode & 0o777);
                } else {
                    tracing::warn!(
                        "auth file has overly permissive permissions ({:o}). Run: chmod 600 {}",
                        mode & 0o777,
                        path.display()
                    );
                }
            }
        }
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| CoraError::ConfigRead(format!("{}: {}", path.display(), e)))?;

    // Simple TOML: expect `[auth]\napi_key = "..."`  or just `api_key = "..."`
    let value: toml::Table = content
        .parse::<toml::Table>()
        .map_err(|e| CoraError::AuthError(format!("invalid TOML: {}", e)))?;

    let key = value
        .get("auth")
        .and_then(|a| a.get("api_key"))
        .and_then(|k| k.as_str())
        .map(std::string::ToString::to_string)
        .or_else(|| {
            value
                .get("api_key")
                .and_then(|k| k.as_str())
                .map(std::string::ToString::to_string)
        });

    Ok(key)
}

/// Save an API key to auth.toml.
pub fn save_api_key(key: &str) -> std::result::Result<(), CoraError> {
    let dir = cora_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| CoraError::AuthError(e.to_string()))?;

    let path = dir.join(AUTH_FILENAME);
    let mut table = toml::Table::new();
    table.insert("api_key".to_string(), toml::Value::String(key.to_string()));
    let content = table.to_string();

    // Create the file with restrictive permissions from the start (0o600),
    // avoiding the TOCTOU window between write and chmod.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
        use std::io::Write;
        file.write_all(content.as_bytes())
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
    }
    #[cfg(not(unix))]
    {
        // Non-Unix platforms: write then attempt to restrict permissions.
        // Best-effort — platform support for file permissions varies.
        std::fs::write(&path, content)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
    }

    debug!(path = %path.display(), "saved API key");

    Ok(())
}

/// Remove the stored API key from auth.toml.
pub fn remove_api_key() -> std::result::Result<(), CoraError> {
    let dir = cora_dir()?;
    let path = dir.join(AUTH_FILENAME);
    if path.is_file() {
        std::fs::remove_file(&path)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
        debug!("removed API key file");
    }
    Ok(())
}

/// Check the auth status: whether an API key is available.
pub fn auth_status() -> std::result::Result<AuthStatus, CoraError> {
    // CORA_API_KEY env var (for CI)
    if std::env::var("CORA_API_KEY").is_ok() {
        return Ok(AuthStatus {
            source: "env var CORA_API_KEY".to_string(),
            has_key: true,
        });
    }
    // auth.toml in cora_data_dir
    if load_api_key_from_auth_file()?.is_some() {
        let dir = cora_dir()?;
        return Ok(AuthStatus {
            source: format!("{}", dir.join(AUTH_FILENAME).display()),
            has_key: true,
        });
    }
    Ok(AuthStatus {
        source: String::new(),
        has_key: false,
    })
}

/// Auth status information.
pub struct AuthStatus {
    pub source: String,
    pub has_key: bool,
}

/// Stored provider information from global config.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub provider: String,
    pub base_url: String,
    pub model: String,
}

/// Save provider info (name, base_url, model) to config.yaml
/// (global config, not secrets).
pub fn save_provider_info(
    provider: &str,
    base_url: &str,
    model: &str,
) -> std::result::Result<(), CoraError> {
    let dir = cora_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| CoraError::AuthError(e.to_string()))?;

    let path = dir.join(GLOBAL_CONFIG_FILENAME);

    // Read existing config.yaml or start fresh
    let mut cora = if path.is_file() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
        CoraFile::from_str(&content).map_err(|e| {
            CoraError::AuthError(format!(
                "cannot parse {}: {}. Fix or remove the file before saving.",
                path.display(),
                e
            ))
        })?
    } else {
        CoraFile::default()
    };

    // Set provider info
    let ps = cora.provider.get_or_insert_with(Default::default);
    ps.provider = Some(provider.to_string());
    ps.model = Some(model.to_string());
    ps.base_url = Some(base_url.to_string());

    let yaml = serde_yaml_ng::to_string(&cora)
        .map_err(|e| CoraError::AuthError(format!("failed to serialize config: {}", e)))?;
    std::fs::write(&path, yaml)
        .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;

    debug!(provider = provider, "saved provider info to config.yaml");
    Ok(())
}

/// Migrate provider info from auth.toml → config.yaml if needed.
/// Called lazily on first load.
fn migrate_provider_info_from_auth() -> std::result::Result<(), CoraError> {
    let dir = cora_dir()?;
    let auth_path = dir.join(AUTH_FILENAME);
    if !auth_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&auth_path)
        .map_err(|e| CoraError::AuthError(format!("{}: {}", auth_path.display(), e)))?;

    let table: toml::Table = match content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };

    // Check if auth.toml has provider info that needs migrating
    let provider = table
        .get("provider")
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    if provider.is_empty() {
        return Ok(()); // No provider info to migrate
    }

    let base_url = table
        .get("base_url")
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let model = table
        .get("model")
        .and_then(toml::Value::as_str)
        .unwrap_or("");

    debug!("migrating provider info from auth.toml → config.yaml");

    // Save to config.yaml
    save_provider_info(provider, base_url, model)?;

    // Strip provider info from auth.toml, keep only api_key
    let mut clean_table = toml::Table::new();
    if let Some(key) = table
        .get("auth")
        .and_then(|a| a.get("api_key"))
        .and_then(|k| k.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            table
                .get("api_key")
                .and_then(|k| k.as_str())
                .map(|s| s.to_string())
        })
    {
        clean_table.insert("api_key".to_string(), toml::Value::String(key));
    }

    if clean_table.is_empty() {
        // No api_key left, remove the file
        let _ = std::fs::remove_file(&auth_path);
    } else {
        let clean_content = clean_table.to_string();
        std::fs::write(&auth_path, clean_content)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", auth_path.display(), e)))?;
        // Re-apply permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&auth_path, perms);
        }
    }

    debug!("migrated provider info from auth.toml → config.yaml");
    Ok(())
}

/// Load stored provider info from config.yaml.
/// Returns `None` if no provider info is stored.
pub fn load_provider_info() -> std::result::Result<Option<ProviderInfo>, CoraError> {
    // Migrate auth.toml provider info → config.yaml if needed
    migrate_provider_info_from_auth()?;

    let dir = cora_dir()?;
    let path = dir.join(GLOBAL_CONFIG_FILENAME);
    if !path.is_file() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;

    let cora = CoraFile::from_str(&content)?;

    let provider = cora
        .provider
        .as_ref()
        .and_then(|p| p.provider.as_deref())
        .unwrap_or("");

    if provider.is_empty() {
        return Ok(None);
    }

    // Load from config.yaml — may need to resolve from preset if model/base_url missing
    let ps = cora.provider.as_ref().unwrap();
    let model = ps.model.as_deref().unwrap_or("");
    let base_url = ps.base_url.as_deref().unwrap_or("");

    // If model/base_url missing, resolve from preset
    let (resolved_model, resolved_base_url) = if model.is_empty() || base_url.is_empty() {
        if let Some(preset) = PRESETS.iter().find(|p| p.name == provider) {
            (
                if model.is_empty() {
                    preset.default_model
                } else {
                    model
                },
                if base_url.is_empty() {
                    preset.default_base_url
                } else {
                    base_url
                },
            )
        } else {
            (model, base_url)
        }
    } else {
        (model, base_url)
    };

    Ok(Some(ProviderInfo {
        provider: provider.to_string(),
        model: resolved_model.to_string(),
        base_url: resolved_base_url.to_string(),
    }))
}

/// Remove stored provider info from config.yaml
/// while keeping other settings.
pub fn remove_provider_info() -> std::result::Result<(), CoraError> {
    let dir = cora_dir()?;
    let path = dir.join(GLOBAL_CONFIG_FILENAME);
    if !path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;

    let mut cora = CoraFile::from_str(&content)?;

    if cora.provider.is_some() {
        cora.provider = None;
        let yaml = serde_yaml_ng::to_string(&cora)
            .map_err(|e| CoraError::AuthError(format!("failed to serialize: {}", e)))?;
        std::fs::write(&path, yaml)
            .map_err(|e| CoraError::AuthError(format!("{}: {}", path.display(), e)))?;
        debug!("removed provider info from config.yaml");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_max_tokens_param_auto_gemini() {
        assert_eq!(
            resolve_max_tokens_param("gemini", "auto"),
            "max_output_tokens"
        );
    }

    #[test]
    fn resolve_max_tokens_param_auto_google() {
        assert_eq!(
            resolve_max_tokens_param("google", "auto"),
            "max_output_tokens"
        );
    }

    #[test]
    fn resolve_max_tokens_param_auto_vertex() {
        assert_eq!(
            resolve_max_tokens_param("vertex", "auto"),
            "max_output_tokens"
        );
    }

    #[test]
    fn resolve_max_tokens_param_auto_openai() {
        assert_eq!(resolve_max_tokens_param("openai", "auto"), "max_tokens");
    }

    #[test]
    fn resolve_max_tokens_param_auto_anthropic() {
        assert_eq!(resolve_max_tokens_param("anthropic", "auto"), "max_tokens");
    }

    #[test]
    fn resolve_max_tokens_param_explicit_override() {
        assert_eq!(
            resolve_max_tokens_param("gemini", "max_tokens"),
            "max_tokens"
        );
        assert_eq!(
            resolve_max_tokens_param("openai", "max_output_tokens"),
            "max_output_tokens"
        );
        assert_eq!(
            resolve_max_tokens_param("anthropic", "max_completion_tokens"),
            "max_completion_tokens"
        );
    }
}
