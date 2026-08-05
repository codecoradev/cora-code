//! Agent configuration read/write module.
//!
//! Provides format-agnostic read, write, merge, and removal operations for
//! AI coding agent config files (JSON, JSONC, YAML).
//!
//! Used by `cora install` to inject the Cora MCP server entry into agent
//! configs without destroying existing data.

use anyhow::{Context, Result};
use std::path::Path;

// ─── Types ───────────────────────────────────────────────────────────

/// Config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Jsonc,
    #[allow(dead_code)]
    Yaml,
}

/// The Cora MCP server entry injected into agent configs.
fn cora_mcp_entry_json() -> serde_json::Value {
    serde_json::json!({
        "command": "cora",
        "args": ["mcp"],
        "description": "Cora Code — AI code review, code intelligence, dead code detection"
    })
}

// ─── Pre-processing helpers ──────────────────────────────────────────

/// Strip a UTF-8 BOM (EF BB BF) if present.
fn strip_bom(input: &str) -> &str {
    input.strip_prefix('\u{FEFF}').unwrap_or(input)
}

/// Strip JSONC comments (`// line` and `/* block */`).
///
/// Uses a state machine that tracks whether the cursor is inside a string
/// literal so that `//` or `/*` inside JSON string values are preserved.
fn strip_jsonc_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            // Handle escape sequences inside strings
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                result.push(bytes[i] as char);
                result.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Not inside a string
        if bytes[i] == b'"' {
            in_string = true;
            result.push('"');
            i += 1;
            continue;
        }

        // Check for line comment //
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // Skip until end of line
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Check for block comment /* */
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2; // skip closing */
            continue;
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Remove trailing commas before `}` or `]` so lenient JSON configs parse.
///
/// Uses a state machine that tracks string context so commas inside string
/// literals are not affected.
fn strip_trailing_commas(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                result.push(bytes[i] as char);
                result.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }

        if bytes[i] == b'"' {
            in_string = true;
            result.push('"');
            i += 1;
            continue;
        }

        // Check for comma followed by optional whitespace then } or ]
        if bytes[i] == b',' {
            // Look ahead past whitespace
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma, keep the whitespace
                i += 1;
                continue;
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Full pre-processing pipeline for JSON/JSONC text.
fn preprocess_json(input: &str) -> String {
    let no_bom = strip_bom(input);
    let no_comments = strip_jsonc_comments(no_bom);
    strip_trailing_commas(&no_comments)
}

// ─── Public API: JSON ────────────────────────────────────────────────

/// Read a JSON/JSONC config file into a `serde_json::Value`.
pub fn read_json_config(path: &Path) -> Result<serde_json::Value> {
    let raw = fs_read(path)?;
    let clean = preprocess_json(&raw);
    serde_json::from_str(&clean)
        .with_context(|| format!("Failed to parse JSON in {}", path.display()))
}

/// Write a JSON config with pretty-printing (2-space indent).
pub fn write_json_config(path: &Path, config: &serde_json::Value) -> Result<()> {
    let output = serde_json::to_string_pretty(config)?;
    fs_write(path, &output)
}

/// Check whether a JSON config already contains a `mcpServers.cora` entry.
pub fn json_has_cora(config: &serde_json::Value) -> bool {
    config
        .get("mcpServers")
        .and_then(|m| m.get("cora"))
        .is_some()
}

/// Insert or overwrite the `mcpServers.cora` entry in a JSON config.
pub fn json_add_cora(config: &mut serde_json::Value) -> Result<()> {
    let obj = config
        .as_object_mut()
        .context("Config root is not a JSON object")?;

    if !obj.contains_key("mcpServers") {
        obj.insert("mcpServers".to_string(), serde_json::json!({}));
    }

    config
        .get_mut("mcpServers")
        .and_then(|m| m.as_object_mut())
        .context("mcpServers is not an object")?
        .insert("cora".to_string(), cora_mcp_entry_json());

    Ok(())
}

/// Remove the `mcpServers.cora` entry. Returns `true` if it was present.
#[allow(dead_code)]
pub fn json_remove_cora(config: &mut serde_json::Value) -> bool {
    if let Some(servers) = config.get_mut("mcpServers").and_then(|m| m.as_object_mut()) {
        servers.remove("cora").is_some()
    } else {
        false
    }
}

// ─── Public API: YAML ────────────────────────────────────────────────

/// Read a YAML config file into a `serde_yaml_ng::Value`.
pub fn read_yaml_config(path: &Path) -> Result<serde_yaml_ng::Value> {
    let raw = fs_read(path)?;
    let no_bom = strip_bom(&raw);
    serde_yaml_ng::from_str(no_bom)
        .with_context(|| format!("Failed to parse YAML in {}", path.display()))
}

/// Write a YAML config.
pub fn write_yaml_config(path: &Path, config: &serde_yaml_ng::Value) -> Result<()> {
    let output = serde_yaml_ng::to_string(config)?;
    fs_write(path, &output)
}

/// Check whether a YAML config already contains a `mcpServers.cora` entry.
pub fn yaml_has_cora(config: &serde_yaml_ng::Value) -> bool {
    config
        .get("mcpServers")
        .and_then(|m| m.get("cora"))
        .is_some()
}

/// Insert or overwrite the `mcpServers.cora` entry in a YAML config.
pub fn yaml_add_cora(config: &mut serde_yaml_ng::Value) -> Result<()> {
    let mapping = config
        .as_mapping_mut()
        .context("Config root is not a YAML mapping")?;

    let key = serde_yaml_ng::Value::String("mcpServers".to_string());
    if !mapping.contains_key(&key) {
        mapping.insert(
            key.clone(),
            serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()),
        );
    }

    let cora_entry: serde_yaml_ng::Value = serde_yaml_ng::from_str(
        "command: cora\nargs:\n  - mcp\ndescription: 'Cora Code — AI code review, code intelligence, dead code detection'",
    )
    .expect("valid yaml");

    config
        .get_mut("mcpServers")
        .and_then(|m| m.as_mapping_mut())
        .context("mcpServers is not a mapping")?
        .insert(serde_yaml_ng::Value::String("cora".to_string()), cora_entry);

    Ok(())
}

/// Remove the `mcpServers.cora` entry from YAML. Returns `true` if present.
#[allow(dead_code)]
pub fn yaml_remove_cora(config: &mut serde_yaml_ng::Value) -> bool {
    if let Some(mapping) = config
        .get_mut("mcpServers")
        .and_then(|m| m.as_mapping_mut())
    {
        let key = serde_yaml_ng::Value::String("cora".to_string());
        mapping.remove(&key).is_some()
    } else {
        false
    }
}

// ─── Format-agnostic helpers ─────────────────────────────────────────

/// Read a config file, dispatching on the detected format.
#[allow(dead_code)]
pub fn read_config(path: &Path, format: ConfigFormat) -> Result<serde_json::Value> {
    match format {
        ConfigFormat::Json | ConfigFormat::Jsonc => read_json_config(path),
        ConfigFormat::Yaml => {
            let yaml = read_yaml_config(path)?;
            // Convert YAML → JSON for a unified return type
            let json_str = serde_yaml_ng::to_string(&yaml)?;
            Ok(serde_json::from_str(&json_str)?)
        }
    }
}

/// Write a config file in the specified format.
#[allow(dead_code)]
pub fn write_config(path: &Path, format: ConfigFormat, config: &serde_json::Value) -> Result<()> {
    match format {
        ConfigFormat::Json | ConfigFormat::Jsonc => write_json_config(path, config),
        ConfigFormat::Yaml => {
            let yaml_str = serde_json::to_string(config)?;
            let yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(&yaml_str)?;
            write_yaml_config(path, &yaml)
        }
    }
}

// ─── File I/O wrappers with context ──────────────────────────────────

fn fs_read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn fs_write(path: &Path, content: &str) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file(name: &str, content: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("cora_test_{}_{}.json", name, std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    // ── BOM stripping ──

    #[test]
    fn strip_bom_removes_utf8_bom() {
        let with_bom = "\u{FEFF}{\"key\": \"value\"}";
        assert_eq!(strip_bom(with_bom), "{\"key\": \"value\"}");
    }

    #[test]
    fn strip_bom_preserves_without_bom() {
        assert_eq!(strip_bom("hello"), "hello");
    }

    // ── JSONC comment stripping ──

    #[test]
    fn strip_line_comments() {
        let input = "{\n  // comment\n  \"key\": \"value\"\n}";
        let result = strip_jsonc_comments(input);
        assert!(!result.contains("comment"));
        assert!(result.contains("\"key\""));
    }

    #[test]
    fn strip_block_comments() {
        let input = "{\n  /* block\n     comment */\n  \"key\": \"value\"\n}";
        let result = strip_jsonc_comments(input);
        assert!(!result.contains("block"));
        assert!(result.contains("\"key\""));
    }

    // ── Trailing comma tolerance ──

    #[test]
    fn strip_trailing_comma_in_object() {
        let input = r#"{"a": 1, "b": 2,}"#;
        let result = strip_trailing_commas(input);
        // The trailing comma after "b": 2 should be removed so JSON parses.
        assert!(!result.contains("2,}"));
        serde_json::from_str::<serde_json::Value>(&result).unwrap();
    }

    #[test]
    fn strip_jsonc_preserves_url_in_string() {
        let input = r#"{"url": "https://example.com"} // trailing comment"#;
        let result = strip_jsonc_comments(input);
        assert!(result.contains("https://example.com"));
        assert!(!result.contains("trailing comment"));
    }

    #[test]
    fn strip_trailing_comma_preserves_comma_in_string() {
        let input = r#"{"key": "a,b}"}"#;
        let result = strip_trailing_commas(input);
        assert!(result.contains("a,b}"));
    }

    #[test]
    fn strip_jsonc_block_comment() {
        let input = r#"{"a": 1 /* block */}"#;
        let result = strip_jsonc_comments(input);
        assert!(!result.contains("block"));
        assert!(result.contains("\"a\""));
    }

    #[test]
    fn strip_trailing_comma_in_array() {
        let input = r#"{"items": [1, 2, 3,]}"#;
        let result = strip_trailing_commas(input);
        serde_json::from_str::<serde_json::Value>(&result).unwrap();
    }

    // ── Full pipeline ──

    #[test]
    fn preprocess_jsonc_with_bom_and_comments() {
        let input = "\u{FEFF}{\n  // hello\n  \"key\": \"value\", /* inline */\n}";
        let result = preprocess_json(input);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    // ── JSON add/remove cora ──

    #[test]
    fn json_add_cora_creates_mcp_servers() {
        let mut config = serde_json::json!({"version": 1});
        json_add_cora(&mut config).unwrap();
        assert!(json_has_cora(&config));
        assert_eq!(config["mcpServers"]["cora"]["command"], "cora");
    }

    #[test]
    fn json_add_cora_preserves_existing_servers() {
        let mut config = serde_json::json!({
            "mcpServers": {
                "other": {"command": "other-tool"}
            }
        });
        json_add_cora(&mut config).unwrap();
        assert!(json_has_cora(&config));
        assert_eq!(config["mcpServers"]["other"]["command"], "other-tool");
    }

    #[test]
    fn json_remove_cora_returns_true_when_present() {
        let mut config = serde_json::json!({
            "mcpServers": {"cora": {"command": "cora"}}
        });
        assert!(json_remove_cora(&mut config));
        assert!(!json_has_cora(&config));
    }

    #[test]
    fn json_remove_cora_returns_false_when_absent() {
        let mut config = serde_json::json!({"mcpServers": {}});
        assert!(!json_remove_cora(&mut config));
    }

    #[test]
    fn json_has_cora_false_without_mcp_servers() {
        let config = serde_json::json!({"version": 1});
        assert!(!json_has_cora(&config));
    }

    // ── YAML add/remove cora ──

    #[test]
    fn yaml_add_cora_creates_mcp_servers() {
        let mut config: serde_yaml_ng::Value = serde_yaml_ng::from_str("version: 1\n").unwrap();
        yaml_add_cora(&mut config).unwrap();
        assert!(yaml_has_cora(&config));
    }

    #[test]
    fn yaml_remove_cora_returns_true_when_present() {
        let mut config: serde_yaml_ng::Value =
            serde_yaml_ng::from_str("mcpServers:\n  cora:\n    command: cora\n").unwrap();
        assert!(yaml_remove_cora(&mut config));
        assert!(!yaml_has_cora(&config));
    }

    #[test]
    fn yaml_has_cora_false_without_mcp_servers() {
        let config: serde_yaml_ng::Value = serde_yaml_ng::from_str("version: 1\n").unwrap();
        assert!(!yaml_has_cora(&config));
    }

    // ── Round-trip: read → add → write → read → verify ──

    #[test]
    fn json_round_trip_preserves_data() {
        let original = r#"{
            "version": 1,
            "mcpServers": {
                "other": {"command": "other"}
            }
        }"#;
        let path = tmp_file("roundtrip", original);
        let result = (|| -> Result<()> {
            let mut config = read_json_config(&path)?;
            json_add_cora(&mut config)?;
            write_json_config(&path, &config)?;

            let reread = read_json_config(&path)?;
            assert!(json_has_cora(&reread));
            assert_eq!(reread["version"], 1);
            assert_eq!(reread["mcpServers"]["other"]["command"], "other");
            Ok(())
        })();
        cleanup(&path);
        result.unwrap();
    }

    #[test]
    fn jsonc_round_trip_with_comments() {
        let original = "\u{FEFF}{\n  // my config\n  \"version\": 1,\n}";
        let path = tmp_file("jsonc", original);
        let result = (|| -> Result<()> {
            let mut config = read_json_config(&path)?;
            assert_eq!(config["version"], 1);
            json_add_cora(&mut config)?;
            write_json_config(&path, &config)?;

            let reread = read_json_config(&path)?;
            assert!(json_has_cora(&reread));
            assert_eq!(reread["version"], 1);
            Ok(())
        })();
        cleanup(&path);
        result.unwrap();
    }

    // ── Nonexistent file ──

    #[test]
    fn read_nonexistent_file_returns_error() {
        let path = std::path::Path::new("/nonexistent/cora_test_432.json");
        let result = read_json_config(path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to read"));
    }
}
