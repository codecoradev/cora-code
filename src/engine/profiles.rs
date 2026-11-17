//! Quality profiles — preset and configurable rule sets per project.
//!
//! Profiles define focus areas, weights, and review behavior that modify
//! the AI review prompt. Built-in profiles are embedded at compile time.
//! Custom profiles can be loaded from `.cora.yaml` or external files.

use serde::{Deserialize, Serialize};
use tracing::debug;

// ─── Built-in profile YAML sources (embedded at compile time) ───

const SECURITY_FIRST_YAML: &str = include_str!("../profiles/security-first.yaml");
const PERFORMANCE_YAML: &str = include_str!("../profiles/performance.yaml");
const CLEAN_CODE_YAML: &str = include_str!("../profiles/clean-code.yaml");
const BEGINNER_FRIENDLY_YAML: &str = include_str!("../profiles/beginner-friendly.yaml");
const MINIMAL_YAML: &str = include_str!("../profiles/minimal.yaml");
const RUST_STRICT_YAML: &str = include_str!("../profiles/rust-strict.yaml");
const TYPESCRIPT_STRICT_YAML: &str = include_str!("../profiles/typescript-strict.yaml");
const GO_PRAGMATIC_YAML: &str = include_str!("../profiles/go-pragmatic.yaml");

/// All built-in profile names in display order.
pub const BUILTIN_PROFILES: &[&str] = &[
    "security-first",
    "performance",
    "clean-code",
    "beginner-friendly",
    "minimal",
    "rust-strict",
    "typescript-strict",
    "go-pragmatic",
];

// ─── Profile types ───

/// A complete quality profile definition.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub focus_areas: Vec<FocusArea>,
    #[serde(default)]
    pub ignore_areas: Vec<String>,
    #[serde(default)]
    pub severity_override: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub review_style: ReviewStyle,
}

/// A single focus area within a profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusArea {
    pub id: String,
    /// Priority weight 1-10 (higher = more important).
    pub weight: u32,
    /// "block" = any finding fails gate, "warn" = report only.
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default)]
    pub rules: Vec<String>,
}

fn default_action() -> String {
    "warn".to_string()
}

/// Review style settings that modify LLM behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewStyle {
    /// "strict" | "standard" | "gentle"
    #[serde(default = "default_tone")]
    pub tone: String,
    /// "minimal" | "standard" | "high" | "exhaustive"
    #[serde(default = "default_detail")]
    pub detail_level: String,
    #[serde(default = "default_true")]
    pub suggest_fixes: bool,
    /// Max findings to report (null = no limit).
    pub max_findings: Option<usize>,
}

fn default_tone() -> String {
    "standard".to_string()
}

fn default_detail() -> String {
    "standard".to_string()
}

fn default_true() -> bool {
    true
}

/// Lightweight profile reference stored in `.cora.yaml`.
/// Can be a simple name string, or a full inline definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProfileRef {
    /// Built-in profile name or path to custom profile file.
    Name(String),
    /// Inline profile definition with optional `extends`.
    Inline(InlineProfileRef),
}

/// Inline profile in `.cora.yaml` — can extend a built-in and override.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InlineProfileRef {
    /// Extend a built-in profile by name.
    #[serde(default)]
    pub extends: Option<String>,
    /// Override focus areas (merged with base).
    #[serde(default)]
    pub focus_areas: Vec<FocusArea>,
    /// Areas to ignore.
    #[serde(default)]
    pub ignore_areas: Vec<String>,
    /// Override review style.
    #[serde(default)]
    pub review_style: Option<ReviewStyle>,
}

// ─── Profile operations ───

impl Profile {
    /// Validate profile values (#81): focus weights must be 1-10, actions and
    /// review-style enums must be recognized. Unknown values fail loudly so a
    /// typo (e.g. `weight: 15` or `action: blok`) does not silently change
    /// review behavior.
    pub fn validate(&self) -> Result<(), String> {
        const VALID_ACTIONS: &[&str] = &["block", "warn", "info"];
        const VALID_TONES: &[&str] = &["strict", "standard", "gentle"];
        const VALID_DETAILS: &[&str] = &["minimal", "standard", "high", "exhaustive"];
        let mut errs: Vec<String> = Vec::new();

        for area in &self.focus_areas {
            if !(1..=10).contains(&area.weight) {
                errs.push(format!(
                    "profile focus area '{}' weight must be 1-10, got: {}",
                    area.id, area.weight
                ));
            }
            if !VALID_ACTIONS.contains(&area.action.as_str()) {
                errs.push(format!(
                    "profile focus area '{}' action must be one of {:?}, got: '{}'",
                    area.id, VALID_ACTIONS, area.action
                ));
            }
        }

        let style = &self.review_style;
        if !style.tone.is_empty() && !VALID_TONES.contains(&style.tone.as_str()) {
            errs.push(format!(
                "profile review_style.tone must be one of {:?}, got: '{}'",
                VALID_TONES, style.tone
            ));
        }
        if !style.detail_level.is_empty() && !VALID_DETAILS.contains(&style.detail_level.as_str()) {
            errs.push(format!(
                "profile review_style.detail_level must be one of {:?}, got: '{}'",
                VALID_DETAILS, style.detail_level
            ));
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }
}

/// Load a built-in profile by name. Returns `None` if not found.
pub fn load_builtin(name: &str) -> Option<Profile> {
    let yaml = match name {
        "security-first" => SECURITY_FIRST_YAML,
        "performance" => PERFORMANCE_YAML,
        "clean-code" => CLEAN_CODE_YAML,
        "beginner-friendly" => BEGINNER_FRIENDLY_YAML,
        "minimal" => MINIMAL_YAML,
        "rust-strict" => RUST_STRICT_YAML,
        "typescript-strict" => TYPESCRIPT_STRICT_YAML,
        "go-pragmatic" => GO_PRAGMATIC_YAML,
        _ => return None,
    };
    parse_profile_yaml(yaml).ok()
}

/// Parse a profile from YAML string.
pub fn parse_profile_yaml(yaml: &str) -> Result<Profile, String> {
    serde_yaml_ng::from_str(yaml).map_err(|e| format!("invalid profile YAML: {e}"))
}

/// Load all built-in profiles.
#[cfg(test)]
pub fn load_all_builtins() -> Vec<Profile> {
    BUILTIN_PROFILES
        .iter()
        .filter_map(|name| load_builtin(name))
        .collect()
}

/// Resolve a `ProfileRef` from `.cora.yaml` into a final `Profile`.
///
/// Resolution order:
/// - If `Name("builtin")` → load built-in
/// - If `Name("./path.yaml")` → load from file (resolve relative to project root)
/// - If `Inline { extends, ... }` → merge with base
/// - If `Inline` without extends → use as-is
pub fn resolve_profile(profile_ref: &ProfileRef) -> Result<Profile, String> {
    resolve_profile_with_root(profile_ref, None)
}

/// Resolve profile with optional project root for relative file path resolution.
pub fn resolve_profile_with_root(
    profile_ref: &ProfileRef,
    project_root: Option<&std::path::Path>,
) -> Result<Profile, String> {
    match profile_ref {
        ProfileRef::Name(name) => {
            // Try built-in first
            if let Some(p) = load_builtin(name) {
                debug!(profile = name, "loaded built-in profile");
                return Ok(p);
            }
            // Resolve relative to project root (not CWD) for deterministic behavior
            let path = if let Some(root) = project_root {
                root.join(name)
            } else {
                std::path::PathBuf::from(name)
            };
            if path.is_file() {
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read profile file '{}': {e}", path.display()))?;
                let mut profile = parse_profile_yaml(&content)?;
                // If profile has no name, derive from filename
                if profile.name.is_empty() {
                    profile.name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("custom")
                        .to_string();
                }
                debug!(profile = %path.display(), "loaded profile from file");
                return Ok(profile);
            }
            Err(format!(
                "unknown profile '{name}'. Available built-ins: {}",
                BUILTIN_PROFILES.join(", ")
            ))
        }
        ProfileRef::Inline(inline) => {
            let mut profile = if let Some(base_name) = &inline.extends {
                load_builtin(base_name)
                    .ok_or_else(|| format!("cannot extend unknown profile '{base_name}'"))?
            } else {
                Profile::default()
            };

            // Override name if extending
            if let Some(ref base) = inline.extends {
                profile.name = format!("custom-{base}");
            } else {
                profile.name = "custom".to_string();
            }

            // Merge focus areas by id — replace existing, append new
            if !inline.focus_areas.is_empty() {
                for area in &inline.focus_areas {
                    if let Some(existing) = profile.focus_areas.iter_mut().find(|a| a.id == area.id)
                    {
                        // Replace existing focus area with override
                        *existing = area.clone();
                    } else {
                        // Append new focus area
                        profile.focus_areas.push(area.clone());
                    }
                }
            }

            // Override ignore areas
            if !inline.ignore_areas.is_empty() {
                profile.ignore_areas = inline.ignore_areas.clone();
            }

            // Override review style
            if let Some(style) = &inline.review_style {
                profile.review_style = style.clone();
            }

            Ok(profile)
        }
    }
}

/// Auto-detect a profile based on project language.
/// Returns the best-matching built-in profile name.
#[cfg(test)]
pub fn auto_detect_profile(language: &str) -> &'static str {
    match language {
        "rs" => "rust-strict",
        "ts" | "js" => "typescript-strict",
        "go" => "go-pragmatic",
        _ => "clean-code",
    }
}

/// Build the profile instruction text that gets injected into the LLM prompt.
///
/// This replaces the generic focus area list with profile-specific instructions.
pub fn build_profile_prompt(profile: &Profile) -> String {
    let mut parts = Vec::new();

    parts.push(format!("Reviewing using the '{}' profile.", profile.name));
    if !profile.description.is_empty() {
        parts.push(profile.description.clone());
    }

    // Focus areas with rules
    if !profile.focus_areas.is_empty() {
        parts.push("\nFOCUSED REVIEW PRIORITIES:".to_string());
        // Sort by weight descending
        let mut areas = profile.focus_areas.clone();
        areas.sort_by_key(|b| std::cmp::Reverse(b.weight));

        for area in &areas {
            let action_label = if area.action == "block" {
                "BLOCK"
            } else {
                "WARN"
            };
            parts.push(format!(
                "\n[{}] {} (weight: {}/10, action: {})",
                area.id.to_uppercase(),
                area.id,
                area.weight,
                action_label
            ));
            for rule in &area.rules {
                parts.push(format!("  - {rule}"));
            }
        }
    }

    // Ignore areas
    if !profile.ignore_areas.is_empty() {
        parts.push(format!(
            "\nIGNORE these areas (do NOT report): {}",
            profile.ignore_areas.join(", ")
        ));
    }

    // Review style
    let style = &profile.review_style;
    if !style.tone.is_empty() && style.tone != "standard" {
        parts.push(format!("\nReview tone: {}.", style.tone));
    }
    if !style.detail_level.is_empty() && style.detail_level != "standard" {
        parts.push(format!("Detail level: {}.", style.detail_level));
    }
    if !style.suggest_fixes {
        parts.push("Do NOT suggest fixes — just report issues.".to_string());
    }
    if let Some(max) = style.max_findings {
        parts.push(format!("Report at most {max} findings."));
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_all_builtins_succeeds() {
        let profiles = load_all_builtins();
        assert_eq!(profiles.len(), 8, "should have 8 built-in profiles");
        for p in &profiles {
            assert!(!p.name.is_empty(), "profile name should not be empty");
            assert!(!p.description.is_empty(), "profile should have description");
        }
    }

    #[test]
    fn load_builtin_security_first() {
        let p = load_builtin("security-first").unwrap();
        assert_eq!(p.name, "security-first");
        assert!(!p.focus_areas.is_empty());
        assert!(p.ignore_areas.contains(&"style".to_string()));
        assert_eq!(p.review_style.tone, "strict");
    }

    #[test]
    fn load_builtin_rust_strict() {
        let p = load_builtin("rust-strict").unwrap();
        assert_eq!(p.name, "rust-strict");
        assert!(p.focus_areas.iter().any(|a| a.id == "unsafe_usage"));
    }

    #[test]
    fn load_builtin_unknown_returns_none() {
        assert!(load_builtin("nonexistent").is_none());
    }

    #[test]
    fn resolve_builtin_name() {
        let r = ProfileRef::Name("minimal".to_string());
        let p = resolve_profile(&r).unwrap();
        assert_eq!(p.name, "minimal");
    }

    #[test]
    fn resolve_unknown_name_fails() {
        let r = ProfileRef::Name("nope".to_string());
        assert!(resolve_profile(&r).is_err());
    }

    #[test]
    fn resolve_inline_without_extends() {
        let r = ProfileRef::Inline(InlineProfileRef {
            extends: None,
            focus_areas: vec![FocusArea {
                id: "test".to_string(),
                weight: 5,
                action: "warn".to_string(),
                rules: vec!["No test rule".to_string()],
            }],
            ignore_areas: vec!["docs".to_string()],
            review_style: None,
        });
        let p = resolve_profile(&r).unwrap();
        assert_eq!(p.name, "custom");
        assert_eq!(p.focus_areas.len(), 1);
        assert_eq!(p.ignore_areas, vec!["docs"]);
    }

    #[test]
    fn resolve_inline_extends_builtin() {
        let r = ProfileRef::Inline(InlineProfileRef {
            extends: Some("minimal".to_string()),
            focus_areas: vec![FocusArea {
                id: "compliance".to_string(),
                weight: 8,
                action: "warn".to_string(),
                rules: vec!["Check data processing consent".to_string()],
            }],
            ignore_areas: vec![],
            review_style: Some(ReviewStyle {
                tone: "strict".to_string(),
                detail_level: "high".to_string(),
                suggest_fixes: true,
                max_findings: Some(15),
            }),
        });
        let p = resolve_profile(&r).unwrap();
        assert_eq!(p.name, "custom-minimal");
        // Should have base focus areas + custom
        assert!(p.focus_areas.len() > 2);
        assert!(p.focus_areas.iter().any(|a| a.id == "compliance"));
        assert_eq!(p.review_style.tone, "strict");
        assert_eq!(p.review_style.max_findings, Some(15));
    }

    #[test]
    fn auto_detect_profiles() {
        assert_eq!(auto_detect_profile("rs"), "rust-strict");
        assert_eq!(auto_detect_profile("ts"), "typescript-strict");
        assert_eq!(auto_detect_profile("js"), "typescript-strict");
        assert_eq!(auto_detect_profile("go"), "go-pragmatic");
        assert_eq!(auto_detect_profile("py"), "clean-code");
        assert_eq!(auto_detect_profile("unknown"), "clean-code");
    }

    #[test]
    fn build_profile_prompt_not_empty() {
        let p = load_builtin("security-first").unwrap();
        let prompt = build_profile_prompt(&p);
        assert!(prompt.contains("security-first"));
        assert!(prompt.contains("FOCUSED REVIEW PRIORITIES"));
        assert!(prompt.contains("IGNORE these areas"));
        assert!(prompt.contains("strict"));
    }

    #[test]
    fn build_profile_prompt_minimal() {
        let p = Profile {
            name: "custom".to_string(),
            ..Default::default()
        };
        let prompt = build_profile_prompt(&p);
        assert!(prompt.contains("custom"));
    }

    #[test]
    fn parse_profile_yaml_invalid() {
        let result = parse_profile_yaml("not: valid: yaml:::");
        assert!(result.is_err());
    }

    #[test]
    fn profile_roundtrip_yaml() {
        let p = load_builtin("performance").unwrap();
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        let back: Profile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.name, p.name);
        assert_eq!(back.focus_areas.len(), p.focus_areas.len());
    }

    // ─── #81: Profile::validate ───

    #[test]
    fn validate_accepts_all_builtins() {
        for name in BUILTIN_PROFILES {
            let p = load_builtin(name).unwrap_or_else(|| panic!("missing builtin {name}"));
            assert!(p.validate().is_ok(), "builtin '{name}' failed validation");
        }
    }

    #[test]
    fn validate_rejects_weight_out_of_range() {
        let p = Profile {
            name: "test".into(),
            focus_areas: vec![FocusArea {
                id: "x".into(),
                weight: 15,
                action: "warn".into(),
                rules: vec![],
            }],
            ..Default::default()
        };
        let err = p.validate().unwrap_err();
        assert!(err.contains("weight"), "err: {err}");
    }

    #[test]
    fn validate_rejects_unknown_action() {
        let p = Profile {
            name: "test".into(),
            focus_areas: vec![FocusArea {
                id: "x".into(),
                weight: 5,
                action: "blok".into(), // typo
                rules: vec![],
            }],
            ..Default::default()
        };
        let err = p.validate().unwrap_err();
        assert!(err.contains("action"), "err: {err}");
    }

    #[test]
    fn validate_rejects_unknown_tone() {
        let p = Profile {
            name: "test".into(),
            review_style: ReviewStyle {
                tone: "angry".into(),
                detail_level: "standard".into(),
                suggest_fixes: true,
                max_findings: None,
            },
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }
}
