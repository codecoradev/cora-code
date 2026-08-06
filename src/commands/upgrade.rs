//! `cora upgrade` — check for updates and self-upgrade.
//!
//! Detects OS/arch, fetches latest release from GitHub, downloads,
//! verifies checksum, replaces the running binary.
//!
//! Uses a blocking tokio runtime for the HTTP calls (reqwest is async-only
//! in cora-code, unlike uteke which uses reqwest::blocking).

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use colored::Colorize;
use sha2::{Digest, Sha256};

const REPO: &str = "codecoradev/cora-code";
const BINARY_NAME: &str = "cora";

/// Entry point for `cora upgrade`.
///
/// `check_only` = true corresponds to `cora upgrade --check`:
/// only print whether an update is available, do not download.
pub async fn run(yes: bool, check_only: bool) -> anyhow::Result<i32> {
    let current_version = env!("CARGO_PKG_VERSION");
    println!("{} Current version: {current_version}", "[INFO]".green());

    // Detect current binary path
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} Cannot determine current binary path: {e}", "[ERROR]".red());
            eprintln!("        If installed via cargo, run: cargo install --path .");
            return Ok(1);
        }
    };

    // Detect OS and architecture
    let os = detect_os();
    let arch = detect_arch();

    // Get latest release version
    let latest_version = match get_latest_version().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            return Ok(1);
        }
    };

    // Normalize: strip leading 'v' from GitHub tag for comparison
    let latest_clean = latest_version.trim_start_matches('v');

    // Check if already up to date
    if latest_clean == current_version {
        println!("{} Already up to date ({current_version})", "[INFO]".green());
        return Ok(0);
    }

    println!("{} Latest version:  {latest_version}", "[INFO]".cyan());
    println!(
        "{} Release notes:  https://github.com/{REPO}/releases/tag/{latest_version}",
        "[INFO]".dimmed()
    );

    if check_only {
        return Ok(0);
    }

    // Confirm (unless --yes)
    if !yes {
        print!("? Update to {latest_version}? [y/N] ");
        io::stdout()
            .flush()
            .map_err(|e| anyhow::anyhow!("stdout flush: {e}"))?;
        let mut input = String::new();
        io::stdin()
            .lock()
            .read_line(&mut input)
            .map_err(|e| anyhow::anyhow!("stdin read: {e}"))?;
        let input = input.trim().to_lowercase();
        if input != "y" && input != "yes" {
            println!("{} Update cancelled.", "[INFO]".dimmed());
            return Ok(0);
        }
    }

    // Build target and download
    let target = match get_target(&os, &arch) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            return Ok(1);
        }
    };

    let archive_name = format!("{BINARY_NAME}-{target}-{latest_version}.tar.gz");
    let download_url =
        format!("https://github.com/{REPO}/releases/download/{latest_version}/{archive_name}");

    println!("{} Downloading {archive_name} ...", "[INFO]".green());

    let temp_dir = std::env::temp_dir().join(format!("cora-update-{latest_version}"));
    fs::create_dir_all(&temp_dir).map_err(|e| anyhow::anyhow!("Failed to create temp dir: {e}"))?;
    let archive_path = temp_dir.join(&archive_name);

    // Download using a blocking tokio runtime (reqwest is async-only)
    let archive_bytes = match download_async(&download_url).await {
        Ok(b) => b,
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_dir);
            eprintln!("{} Download failed: {e}", "[ERROR]".red());
            return Ok(1);
        }
    };

    fs::write(&archive_path, &archive_bytes).map_err(|e| anyhow::anyhow!("Failed to write archive: {e}"))?;

    // Verify checksum
    let checksums_url = format!(
        "https://github.com/{REPO}/releases/download/{latest_version}/checksums-sha256.txt"
    );

    println!("{} Verifying checksum ...", "[INFO]".green());

    let skip_checksum = std::env::var("CORA_UPGRADE_SKIP_CHECKSUM")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    if skip_checksum {
        println!(
            "{} Checksum verification skipped (CORA_UPGRADE_SKIP_CHECKSUM=1)",
            "[WARN]".yellow()
        );
    } else {
        let checksums_text = match download_async(&checksums_url).await {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(e) => {
                let _ = fs::remove_dir_all(&temp_dir);
                eprintln!("{} Failed to download checksums: {e}", "[ERROR]".red());
                eprintln!("        Set CORA_UPGRADE_SKIP_CHECKSUM=1 to skip.");
                return Ok(1);
            }
        };

        let expected = match parse_checksum(&checksums_text, &archive_name) {
            Some(h) => h,
            None => {
                let _ = fs::remove_dir_all(&temp_dir);
                eprintln!(
                    "{} Checksum for '{archive_name}' not found in checksums file.",
                    "[ERROR]".red()
                );
                eprintln!("        Set CORA_UPGRADE_SKIP_CHECKSUM=1 to bypass.");
                return Ok(1);
            }
        };

        let actual = sha256_file(&archive_path)?;
        if actual != expected {
            let _ = fs::remove_dir_all(&temp_dir);
            eprintln!(
                "{} Checksum mismatch! Expected: {expected}, got: {actual}",
                "[ERROR]".red()
            );
            return Ok(1);
        }
        println!("{} Checksum verified: {actual}", "[INFO]".green());
    }

    // Verify archive integrity (path traversal check)
    let file = fs::File::open(&archive_path).map_err(|e| anyhow::anyhow!("Failed to open archive: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries().map_err(|e| anyhow::anyhow!("Failed to read archive entries: {e}"))? {
        let entry = entry.map_err(|e| anyhow::anyhow!("Failed to read archive entry: {e}"))?;
        let path = entry.path().map_err(|e| anyhow::anyhow!("Archive path error: {e}"))?;
        let path_str = path.to_string_lossy();
        if path_str.starts_with('/') || path_str.contains("..") {
            let _ = fs::remove_dir_all(&temp_dir);
            eprintln!(
                "{} Archive contains unsafe paths — refusing to extract",
                "[ERROR]".red()
            );
            return Ok(1);
        }
    }
    drop(archive);

    // Extract
    println!("{} Extracting ...", "[INFO]".green());
    let file = fs::File::open(&archive_path).map_err(|e| anyhow::anyhow!("Failed to open archive: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(&temp_dir).map_err(|e| anyhow::anyhow!("Failed to extract archive: {e}"))?;

    // Find and replace binary
    let extracted_binary = temp_dir.join(BINARY_NAME);
    if !extracted_binary.exists() {
        let _ = fs::remove_dir_all(&temp_dir);
        eprintln!(
            "{} Binary '{BINARY_NAME}' not found in archive",
            "[ERROR]".red()
        );
        return Ok(1);
    }

    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine install directory"))?;

    // Copy to temp file first, then rename (atomic on POSIX)
    let temp_new = install_dir.join(format!("{BINARY_NAME}.new"));
    fs::copy(&extracted_binary, &temp_new).map_err(|e| anyhow::anyhow!("Failed to copy new binary: {e}"))?;

    // Verify the new binary runs
    match std::process::Command::new(&temp_new).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let new_version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let extracted_version =
                new_version.split_whitespace().nth(1).unwrap_or("unknown");
            println!("{} Verified new binary: {extracted_version}", "[INFO]".green());
        }
        Ok(output) => {
            let _ = fs::remove_file(&temp_new);
            let _ = fs::remove_dir_all(&temp_dir);
            eprintln!(
                "{} New binary failed to run: {}",
                "[ERROR]".red(),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(1);
        }
        Err(e) => {
            let _ = fs::remove_file(&temp_new);
            let _ = fs::remove_dir_all(&temp_dir);
            eprintln!("{} Failed to verify new binary: {e}", "[ERROR]".red());
            return Ok(1);
        }
    }

    // Atomic rename
    fs::rename(&temp_new, &current_exe).map_err(|e| anyhow::anyhow!("Failed to replace binary: {e}"))?;

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);

    println!(
        "{} Update complete. ({current_version} → {latest_version})",
        "[INFO]".green().bold()
    );

    Ok(0)
}

/// Download a URL using the current tokio runtime.
///
/// reqwest in cora-code is async-only (no `blocking` feature).
/// This must be called from within a tokio runtime context.
async fn download_async(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| anyhow::anyhow!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("HTTP {status}: {body}");
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read response body: {e}"))?;
    Ok(bytes.to_vec())
}

fn detect_os() -> String {
    match std::env::consts::OS {
        "linux" => "linux".to_string(),
        "macos" => "darwin".to_string(),
        os => os.to_string(),
    }
}

fn detect_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64".to_string(),
        "aarch64" => "aarch64".to_string(),
        arch => arch.to_string(),
    }
}

fn get_target(os: &str, arch: &str) -> Result<String, String> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu".into()),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu".into()),
        ("darwin", "aarch64") => Ok("aarch64-apple-darwin".into()),
        ("darwin", "x86_64") => Ok("x86_64-apple-darwin".into()),
        _ => Err(format!("Unsupported platform: {os} {arch}")),
    }
}

/// Get latest release tag from GitHub.
///
/// Primary: parse 302 redirect (no API call, no rate limit).
/// Fallback: GitHub REST API.
async fn get_latest_version() -> Result<String, String> {
    let client = reqwest::Client::new();

    // Primary: HEAD request, parse Location header redirect
    let resp = client
        .head(format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .await
        .map_err(|e| format!("Failed to check latest release: {e}"))?;

    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or_default();
        // Redirect URL: https://github.com/codecoradev/cora-code/releases/tag/v0.14.0
        if let Some(tag) = loc.rsplit('/').next() {
            if tag.starts_with('v') {
                return Ok(tag.trim_end_matches('?').to_string());
            }
        }
    }

    // Fallback: GitHub API
    let api_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client
        .get(&api_url)
        .header("User-Agent", "cora-upgrade")
        .send()
        .await
        .map_err(|e| format!("GitHub API failed: {e}"))?;

    if resp.status().is_success() {
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;
        if let Some(tag) = json["tag_name"].as_str() {
            return Ok(tag.to_string());
        }
    }

    Err(format!(
        "Failed to determine latest version. Check https://github.com/{REPO}/releases"
    ))
}

fn parse_checksum(checksums_text: &str, archive_name: &str) -> Option<String> {
    for line in checksums_text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1].contains(archive_name) {
            return Some(parts[0].to_string());
        }
    }
    None
}

fn sha256_file(path: &PathBuf) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path).map_err(|e| anyhow::anyhow!("Failed to open file: {e}"))?;
    io::copy(&mut file, &mut hasher).map_err(|e| anyhow::anyhow!("Failed to read file: {e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

