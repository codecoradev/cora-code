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

/// Maximum accepted size for a release archive (compressed).
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum accepted size for the checksums file / API JSON.
const MAX_SMALL_BYTES: u64 = 1024 * 1024;
/// Maximum accepted size for the extracted binary.
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

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
            eprintln!(
                "{} Cannot determine current binary path: {e}",
                "[ERROR]".red()
            );
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
        println!(
            "{} Already up to date ({current_version})",
            "[INFO]".green()
        );
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

    // Random, 0700 temp dir (removed on drop, including early returns).
    let temp_guard =
        tempfile::tempdir().map_err(|e| anyhow::anyhow!("Failed to create temp dir: {e}"))?;
    let temp_dir = temp_guard.path().to_path_buf();
    let archive_path = temp_dir.join(&archive_name);

    // Download using a blocking tokio runtime (reqwest is async-only)
    let archive_bytes = match download_async(&download_url, MAX_ARCHIVE_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} Download failed: {e}", "[ERROR]".red());
            return Ok(1);
        }
    };

    fs::write(&archive_path, &archive_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to write archive: {e}"))?;

    // Verify checksum
    let checksums_url = format!(
        "https://github.com/{REPO}/releases/download/{latest_version}/checksums-sha256.txt"
    );

    println!("{} Verifying checksum ...", "[INFO]".green());

    let skip_checksum = match skip_checksum_decision(
        std::env::var("CORA_UPGRADE_SKIP_CHECKSUM").ok().as_deref(),
        std::env::var("CORA_UPGRADE_I_UNDERSTAND").ok().as_deref(),
    ) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{} {msg}", "[ERROR]".red());
            return Ok(1);
        }
    };

    if skip_checksum {
        eprintln!(
            "{} !!! CHECKSUM VERIFICATION DISABLED (CORA_UPGRADE_SKIP_CHECKSUM + CORA_UPGRADE_I_UNDERSTAND) !!!",
            "[WARN]".yellow().bold()
        );
        eprintln!(
            "{} The downloaded binary is NOT verified and will replace the running one.",
            "[WARN]".yellow().bold()
        );
    } else {
        let checksums_text = match download_async(&checksums_url, MAX_SMALL_BYTES).await {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(e) => {
                eprintln!("{} Failed to download checksums: {e}", "[ERROR]".red());
                eprintln!(
                    "        (Unsafe bypass: CORA_UPGRADE_SKIP_CHECKSUM=1 CORA_UPGRADE_I_UNDERSTAND=1)"
                );
                return Ok(1);
            }
        };

        let expected = match parse_checksum(&checksums_text, &archive_name) {
            Some(h) => h,
            None => {
                eprintln!(
                    "{} Checksum for '{archive_name}' not found in checksums file.",
                    "[ERROR]".red()
                );
                eprintln!(
                    "        (Unsafe bypass: CORA_UPGRADE_SKIP_CHECKSUM=1 CORA_UPGRADE_I_UNDERSTAND=1)"
                );
                return Ok(1);
            }
        };

        let actual = sha256_file(&archive_path)?;
        if actual != expected {
            eprintln!(
                "{} Checksum mismatch! Expected: {expected}, got: {actual}",
                "[ERROR]".red()
            );
            return Ok(1);
        }
        println!("{} Checksum verified: {actual}", "[INFO]".green());
    }

    // Extract only the single regular-file binary entry; reject links.
    println!("{} Extracting ...", "[INFO]".green());
    let extracted_binary = match extract_binary(&archive_path, &temp_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {e}", "[ERROR]".red());
            return Ok(1);
        }
    };

    let install_dir = current_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine install directory"))?;

    // Copy to temp file first, then rename (atomic on POSIX)
    let temp_new = install_dir.join(format!("{BINARY_NAME}.new"));
    fs::copy(&extracted_binary, &temp_new)
        .map_err(|e| anyhow::anyhow!("Failed to copy new binary: {e}"))?;

    // Verify the new binary runs
    match std::process::Command::new(&temp_new)
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let new_version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let extracted_version = new_version.split_whitespace().nth(1).unwrap_or("unknown");
            println!(
                "{} Verified new binary: {extracted_version}",
                "[INFO]".green()
            );
        }
        Ok(output) => {
            let _ = fs::remove_file(&temp_new);
            eprintln!(
                "{} New binary failed to run: {}",
                "[ERROR]".red(),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(1);
        }
        Err(e) => {
            let _ = fs::remove_file(&temp_new);
            eprintln!("{} Failed to verify new binary: {e}", "[ERROR]".red());
            return Ok(1);
        }
    }

    // Atomic rename
    fs::rename(&temp_new, &current_exe)
        .map_err(|e| anyhow::anyhow!("Failed to replace binary: {e}"))?;

    // Cleanup

    println!(
        "{} Update complete. ({current_version} → {latest_version})",
        "[INFO]".green().bold()
    );

    Ok(0)
}

fn http_client(follow_redirects: bool) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent("cora-upgrade");
    if !follow_redirects {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    b.build()
        .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {e}"))
}

/// Download a URL with timeouts and a hard cap on the body size.
async fn download_async(url: &str, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let mut resp = http_client(true)?
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("HTTP request failed: {e}"))?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            anyhow::bail!("Response too large ({len} bytes, limit {max_bytes})");
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read response body: {e}"))?
    {
        if buf.len() as u64 + chunk.len() as u64 > max_bytes {
            anyhow::bail!("Response exceeded size limit of {max_bytes} bytes");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Decide whether checksum verification may be skipped.
///
/// Requires BOTH `CORA_UPGRADE_SKIP_CHECKSUM` (1/true) and the explicit
/// acknowledgement `CORA_UPGRADE_I_UNDERSTAND` (1/true). The skip variable
/// alone is an error rather than a silent downgrade.
fn skip_checksum_decision(skip: Option<&str>, ack: Option<&str>) -> Result<bool, String> {
    let on = |v: Option<&str>| matches!(v, Some("1") | Some("true"));
    if !on(skip) {
        return Ok(false);
    }
    if on(ack) {
        Ok(true)
    } else {
        Err("CORA_UPGRADE_SKIP_CHECKSUM is set but unsafe skipping also requires              CORA_UPGRADE_I_UNDERSTAND=1. Refusing to continue."
            .to_string())
    }
}

/// A release tag must look like `vX.Y.Z[-suffix]` (safe chars only) since it is
/// interpolated into download URLs.
fn is_valid_tag(tag: &str) -> bool {
    tag.len() <= 64
        && tag.starts_with('v')
        && tag.len() > 1
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Extract only the top-level `cora` regular file from the tarball into `dest_dir`.
///
/// Any symlink/hardlink entry anywhere in the archive causes the whole archive
/// to be rejected, as do unsafe paths.
fn extract_binary(
    archive_path: &std::path::Path,
    dest_dir: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let file =
        fs::File::open(archive_path).map_err(|e| anyhow::anyhow!("Failed to open archive: {e}"))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let dest = dest_dir.join(BINARY_NAME);
    let mut found = false;

    for entry in archive
        .entries()
        .map_err(|e| anyhow::anyhow!("Failed to read archive entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| anyhow::anyhow!("Failed to read archive entry: {e}"))?;
        let ty = entry.header().entry_type();
        let path = entry
            .path()
            .map_err(|e| anyhow::anyhow!("Archive path error: {e}"))?
            .into_owned();
        let path_str = path.to_string_lossy().to_string();

        if path_str.starts_with('/')
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("Archive contains unsafe paths - refusing to extract");
        }
        if ty.is_symlink() || ty.is_hard_link() {
            anyhow::bail!(
                "Archive contains a symlink/hardlink entry ('{path_str}') - refusing to extract"
            );
        }
        if path_str.trim_start_matches("./") == BINARY_NAME {
            if !ty.is_file() {
                anyhow::bail!("Archive entry '{BINARY_NAME}' is not a regular file");
            }
            if found {
                anyhow::bail!("Archive contains duplicate '{BINARY_NAME}' entries");
            }
            if entry.size() > MAX_BINARY_BYTES {
                anyhow::bail!("Binary in archive is too large ({} bytes)", entry.size());
            }
            entry
                .unpack(&dest)
                .map_err(|e| anyhow::anyhow!("Failed to extract binary: {e}"))?;
            found = true;
        }
    }

    if !found {
        anyhow::bail!("Binary '{BINARY_NAME}' not found in archive");
    }
    Ok(dest)
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
    let probe = http_client(false).map_err(|e| e.to_string())?;
    let client = http_client(true).map_err(|e| e.to_string())?;

    // Primary: HEAD request, parse Location header redirect (not followed)
    let resp = probe
        .head(format!("https://github.com/{REPO}/releases/latest"))
        .send()
        .await
        .map_err(|e| format!("Failed to check latest release: {e}"))?;

    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or_default();
        // Redirect URL: https://github.com/codecoradev/cora-code/releases/tag/v0.14.0
        if let Some(tag) = loc.rsplit('/').next() {
            let tag = tag.trim_end_matches('?');
            if is_valid_tag(tag) {
                return Ok(tag.to_string());
            }
        }
    }

    // Fallback: GitHub API
    let api_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client
        .get(&api_url)
        .send()
        .await
        .map_err(|e| format!("GitHub API failed: {e}"))?;

    if resp.status().is_success() {
        if resp.content_length().is_some_and(|l| l > MAX_SMALL_BYTES) {
            return Err("GitHub API response too large".to_string());
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read GitHub API response: {e}"))?;
        if body.len() as u64 > MAX_SMALL_BYTES {
            return Err("GitHub API response too large".to_string());
        }
        let json: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;
        if let Some(tag) = json["tag_name"].as_str() {
            if is_valid_tag(tag) {
                return Ok(tag.to_string());
            }
        }
    }

    Err(format!(
        "Failed to determine latest version. Check https://github.com/{REPO}/releases"
    ))
}

fn parse_checksum(checksums_text: &str, archive_name: &str) -> Option<String> {
    for line in checksums_text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() == 2 {
            let name = parts[1].trim_start_matches('*');
            let name = name.strip_prefix("./").unwrap_or(name);
            if name == archive_name {
                return Some(parts[0].to_ascii_lowercase());
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_exact_match_only() {
        let t = "aaa  cora-x-v1.tar.gz.sig\nbbb  evil-cora-x-v1.tar.gz\nccc  *cora-x-v1.tar.gz\n";
        assert_eq!(
            parse_checksum(t, "cora-x-v1.tar.gz").as_deref(),
            Some("ccc")
        );
        assert_eq!(
            parse_checksum("ddd  ./cora-x-v1.tar.gz", "cora-x-v1.tar.gz").as_deref(),
            Some("ddd")
        );
        assert_eq!(
            parse_checksum("aaa  cora-x-v1.tar.gz.sig", "cora-x-v1.tar.gz"),
            None
        );
        assert_eq!(
            parse_checksum("bbb  evil-cora-x-v1.tar.gz", "cora-x-v1.tar.gz"),
            None
        );
    }

    #[test]
    fn skip_checksum_requires_ack() {
        assert_eq!(skip_checksum_decision(None, None), Ok(false));
        assert_eq!(skip_checksum_decision(None, Some("1")), Ok(false));
        assert!(skip_checksum_decision(Some("1"), None).is_err());
        assert!(skip_checksum_decision(Some("true"), Some("no")).is_err());
        assert_eq!(skip_checksum_decision(Some("1"), Some("1")), Ok(true));
    }

    #[test]
    fn tag_validation() {
        assert!(is_valid_tag("v0.14.0"));
        assert!(is_valid_tag("v1.0.0-rc.1"));
        assert!(!is_valid_tag("0.14.0"));
        assert!(!is_valid_tag("v1/../x"));
        assert!(!is_valid_tag("v1?a=b"));
        assert!(!is_valid_tag("v"));
    }

    fn build_tar(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, ty, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*ty);
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            if let Some(l) = link {
                h.set_link_name(l).unwrap();
            }
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        let raw = b.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap()
    }

    fn write_archive(dir: &std::path::Path, bytes: &[u8]) -> PathBuf {
        let p = dir.join("a.tar.gz");
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn extract_regular_binary_only() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let tgz = build_tar(&[
            ("README", tar::EntryType::Regular, b"hi", None),
            ("cora", tar::EntryType::Regular, b"BIN", None),
        ]);
        let a = write_archive(d.path(), &tgz);
        let p = extract_binary(&a, out.path()).unwrap();
        assert_eq!(fs::read(p).unwrap(), b"BIN");
        assert!(!out.path().join("README").exists());
    }

    #[test]
    fn extract_rejects_symlink_and_hardlink() {
        for ty in [tar::EntryType::Symlink, tar::EntryType::Link] {
            let d = tempfile::tempdir().unwrap();
            let out = tempfile::tempdir().unwrap();
            let tgz = build_tar(&[
                ("evil", ty, b"", Some("/etc/passwd")),
                ("cora", tar::EntryType::Regular, b"BIN", None),
            ]);
            let a = write_archive(d.path(), &tgz);
            assert!(extract_binary(&a, out.path()).is_err());
        }
    }

    #[test]
    fn extract_rejects_symlink_named_cora_and_missing_binary() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let tgz = build_tar(&[("cora", tar::EntryType::Symlink, b"", Some("/bin/sh"))]);
        let a = write_archive(d.path(), &tgz);
        assert!(extract_binary(&a, out.path()).is_err());

        let tgz = build_tar(&[("other", tar::EntryType::Regular, b"x", None)]);
        let a = write_archive(d.path(), &tgz);
        assert!(extract_binary(&a, out.path()).is_err());
    }
}
