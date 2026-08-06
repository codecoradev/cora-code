//! Background update notification — non-blocking check on startup.
//!
//! When `cora` runs any command, this module checks (in a background thread)
//! whether a newer release exists on GitHub. Results are cached for 24 hours
//! at `~/.codecora/cora-code/update-cache.json` to avoid rate-limiting.
//!
//! The notification is printed to stderr after the main command finishes,
//! so it never interferes with command output or piping.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::data_dir;

const REPO: &str = "codecoradev/cora-code";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours

#[derive(Serialize, Deserialize)]
struct UpdateCache {
    latest_version: String,
    has_update: bool,
    checked_at: u64,
}

/// Spawn a background thread to check for updates.
///
/// Call this right after `Cli::parse()`, before command dispatch.
/// The thread is detached — it will print to stderr when done.
///
/// `enabled` = false (from config `update_check: false`) skips the check entirely.
pub fn spawn_background_check(enabled: bool) {
    if !enabled {
        return;
    }

    // Check cache first — if fresh (< 24h), just print cached result
    if let Some(cache) = read_cache() {
        if cache.has_update {
            print_update_banner(&cache.latest_version);
        }
        return; // Cache is still fresh, no need to re-check
    }

    let current_version = env!("CARGO_PKG_VERSION").to_string();

    std::thread::spawn(move || {
        let latest = match fetch_latest_version() {
            Ok(v) => v,
            Err(_) => return, // Silent — never show error for background check
        };

        let has_update = latest != current_version;

        // Write cache
        write_cache(&latest, has_update);

        if has_update {
            print_update_banner(&latest);
        }
    });
}

fn print_update_banner(latest: &str) {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!();
    eprintln!("  ⚠ Update available: {}  (currently {})", latest, current);
    eprintln!("    Run `cora upgrade` to update.");
    eprintln!("    https://github.com/{}/releases/tag/{}", REPO, latest);
    eprintln!();
}

fn cache_path() -> PathBuf {
    data_dir::update_cache_path()
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path();
    let content = fs::read_to_string(&path).ok()?;
    let cache: UpdateCache = serde_json::from_str(&content).ok()?;

    // Check freshness
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();

    if now.saturating_sub(cache.checked_at) > CACHE_TTL_SECS {
        return None; // Expired
    }

    Some(cache)
}

fn write_cache(latest_version: &str, has_update: bool) {
    // Ensure data dir exists
    let _ = data_dir::ensure_data_dir();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = UpdateCache {
        latest_version: latest_version.to_string(),
        has_update,
        checked_at: now,
    };

    let path = cache_path();
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = fs::write(&path, json);
    }
}

/// Fetch latest release tag from GitHub.
///
/// Uses 302 redirect parsing (no API rate limit).
fn fetch_latest_version() -> Result<String, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| format!("Runtime error: {e}"))?;

    rt.block_on(async {
        let client = reqwest::Client::new();

        let resp = client
            .head(format!("https://github.com/{REPO}/releases/latest"))
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        if let Some(location) = resp.headers().get("location") {
            let loc = location.to_str().unwrap_or_default();
            if let Some(tag) = loc.rsplit('/').next() {
                if tag.starts_with('v') {
                    return Ok(tag.trim_end_matches('?').to_string());
                }
            }
        }

        Err("Failed to parse latest version from redirect".into())
    })
}
