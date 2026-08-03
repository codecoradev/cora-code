use crate::engine::Severity;
use crate::engine::bundling::types::BundlingConfig;
use crate::engine::rules::types::RulesConfig;
use crate::error::CoraError;
use serde::{Deserialize, Serialize};

/// Runtime configuration — merged from defaults + .cora.yaml + CLI flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Provider configuration.
    pub provider: ProviderConfig,
    /// Focus areas for review.
    pub focus: Vec<String>,
    /// Custom review rules.
    pub rules: Vec<String>,
    /// Ignore configuration.
    pub ignore: IgnoreConfig,
    /// Hook configuration.
    pub hook: HookConfig,
    /// Quality gate configuration.
    pub quality_gate: crate::engine::quality_gate::QualityGateConfig,
    /// Output configuration.
    pub output: OutputConfig,
    /// Response format for LLM API calls ("none" or "`json_object`").
    pub response_format: String,
    /// Optional custom system prompt that replaces the default for review.
    pub review_system_prompt_override: Option<String>,
    /// Optional custom system prompt file path for review.
    pub review_system_prompt_file: Option<String>,
    /// Optional custom system prompt that replaces the default for scan.
    pub scan_system_prompt_override: Option<String>,
    /// Optional custom system prompt file path for scan.
    pub scan_system_prompt_file: Option<String>,
    /// LLM temperature for deterministic output.
    pub temperature: f32,
    /// Max tokens for LLM responses.
    pub max_tokens: u32,
    /// JSON parameter name for max tokens (e.g. "max_tokens" or "max_output_tokens").
    pub max_tokens_param: String,
    /// HTTP timeout in seconds for LLM requests.
    pub timeout: u64,
    /// Cache TTL in minutes for review caching.
    pub cache_ttl: u64,
    /// Static analysis context injection for reviews.
    pub static_analysis: StaticAnalysisConfig,
    /// Rule engine configuration.
    pub rules_config: RulesConfig,
    /// Context chain configuration — cross-file dependency extraction.
    pub context_chain: crate::engine::context::types::ContextConfig,
    /// File bundling configuration for scan/review grouping.
    pub bundling: BundlingConfig,
    /// Debt tracking configuration.
    pub debt: crate::engine::debt_tracker::DebtConfig,
    /// Active quality profile (resolved from .cora.yaml).
    pub profile: Option<crate::engine::profiles::Profile>,
    /// Analysis configuration — dead-code detection, entry-point patterns.
    #[serde(default, skip_serializing_if = "is_default")]
    pub analysis: AnalysisConfig,
}

/// Provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub provider: String,
    pub model: String,
    pub base_url: String,
}

/// Ignore configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IgnoreConfig {
    pub files: Vec<String>,
    pub rules: Vec<String>,
}

/// Hook configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    pub mode: String,
    pub min_severity: String,
    pub max_diff_size: usize,
    pub on_violation: String, // "warn" | "disallow"
}

/// Output configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub format: String,
    pub color: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
            },
            focus: vec![
                "security".into(),
                "performance".into(),
                "bugs".into(),
                "best_practice".into(),
            ],
            rules: vec![],
            ignore: IgnoreConfig {
                files: vec![
                    "node_modules/**".into(),
                    "dist/**".into(),
                    "target/**".into(),
                    ".git/**".into(),
                ],
                rules: vec![],
            },
            hook: HookConfig {
                mode: "warn".to_string(),
                min_severity: "major".to_string(),
                max_diff_size: 5 * 1024 * 1024,
                on_violation: "warn".to_string(),
            },
            quality_gate: crate::engine::quality_gate::QualityGateConfig::default(),
            output: OutputConfig {
                format: "pretty".to_string(),
                color: true,
            },
            response_format: "none".to_string(),
            review_system_prompt_override: None,
            review_system_prompt_file: None,
            scan_system_prompt_override: None,
            scan_system_prompt_file: None,
            temperature: 0.0,
            max_tokens: 4096,
            max_tokens_param: "auto".to_string(),
            timeout: 600,
            cache_ttl: 1440, // 24h in minutes
            static_analysis: StaticAnalysisConfig::default(),
            rules_config: RulesConfig::default(),
            context_chain: crate::engine::context::types::ContextConfig::default(),
            bundling: BundlingConfig::default(),
            debt: crate::engine::debt_tracker::DebtConfig::default(),
            profile: None,
            analysis: AnalysisConfig::default(),
        }
    }
}

impl HookConfig {
    /// Parse the `min_severity` string into a Severity enum.
    pub fn min_severity_level(&self) -> Severity {
        Severity::from_str_lossy(&self.min_severity)
    }
}

impl Config {
    /// Validate semantic constraints on resolved config values (#94).
    ///
    /// Catches typos and out-of-range values that would otherwise propagate
    /// silently to runtime (e.g. `temperature: 5.0`, an unsupported output
    /// format, or a misspelled `max_tokens_param`). Called after merging all
    /// config sources in `loader::load_config`.
    pub fn validate(&self) -> std::result::Result<(), CoraError> {
        let mut errs: Vec<String> = Vec::new();

        // ── provider ──
        if self.provider.provider.trim().is_empty() {
            errs.push("provider.provider must not be empty".into());
        }
        let base = self.provider.base_url.trim();
        let valid_scheme = base.is_empty()
            || base.starts_with("http://")
            || base.starts_with("https://")
            || base.starts_with("ws://")
            || base.starts_with("unix:");
        if !valid_scheme {
            errs.push(format!(
                "provider.base_url must be an http(s) URL, got: {}",
                self.provider.base_url
            ));
        }

        // ── llm ──
        if !(0.0..=2.0).contains(&self.temperature) {
            errs.push(format!(
                "llm.temperature must be between 0.0 and 2.0, got: {}",
                self.temperature
            ));
        }
        if self.max_tokens == 0 {
            errs.push("llm.max_tokens must be at least 1".into());
        }
        if self.timeout == 0 {
            errs.push("llm.timeout must be at least 1 second".into());
        }
        const VALID_TOKEN_PARAMS: &[&str] = &[
            "auto",
            "max_tokens",
            "max_output_tokens",
            "max_completion_tokens",
        ];
        if !VALID_TOKEN_PARAMS.contains(&self.max_tokens_param.as_str()) {
            errs.push(format!(
                "llm.max_tokens_param must be one of {:?}, got: {}",
                VALID_TOKEN_PARAMS, self.max_tokens_param
            ));
        }

        // ── response format ──
        const VALID_RESPONSE_FORMATS: &[&str] = &["none", "json_object"];
        if !VALID_RESPONSE_FORMATS.contains(&self.response_format.as_str()) {
            errs.push(format!(
                "review.response_format must be one of {:?}, got: {}",
                VALID_RESPONSE_FORMATS, self.response_format
            ));
        }

        // ── output ──
        const VALID_OUTPUT_FORMATS: &[&str] = &["pretty", "json", "compact", "sarif"];
        if !VALID_OUTPUT_FORMATS.contains(&self.output.format.as_str()) {
            errs.push(format!(
                "output.format must be one of {:?}, got: {}",
                VALID_OUTPUT_FORMATS, self.output.format
            ));
        }

        // ── hook ──
        const VALID_HOOK_MODES: &[&str] = &["warn", "block"];
        if !VALID_HOOK_MODES.contains(&self.hook.mode.as_str()) {
            errs.push(format!(
                "hook.mode must be one of {:?}, got: {}",
                VALID_HOOK_MODES, self.hook.mode
            ));
        }
        const VALID_VIOLATION: &[&str] = &["warn", "disallow"];
        if !VALID_VIOLATION.contains(&self.hook.on_violation.as_str()) {
            errs.push(format!(
                "hook.on_violation must be one of {:?}, got: {}",
                VALID_VIOLATION, self.hook.on_violation
            ));
        }
        const VALID_SEVERITIES: &[&str] = &["critical", "major", "minor", "info"];
        if !VALID_SEVERITIES.contains(&self.hook.min_severity.to_lowercase().as_str()) {
            errs.push(format!(
                "hook.min_severity must be one of {:?}, got: {}",
                VALID_SEVERITIES, self.hook.min_severity
            ));
        }

        // ── profile (#81) ──
        if let Some(profile) = &self.profile {
            if let Err(pe) = profile.validate() {
                errs.push(pe);
            }
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(CoraError::ConfigParse(format!(
                "invalid configuration:\n  - {}",
                errs.join("\n  - ")
            )))
        }
    }
}

/// Serde-compatible schema for the `.cora.yaml` configuration file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CoraFile {
    #[serde(
        default,
        deserialize_with = "deserialize_provider_section",
        skip_serializing_if = "Option::is_none"
    )]
    pub provider: Option<ProviderSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<IgnoreSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook: Option<HookSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_gate: Option<crate::engine::quality_gate::QualityGateConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules_engine: Option<RulesSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundling: Option<BundlingSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debt: Option<crate::engine::debt_tracker::DebtConfig>,
    #[serde(default)]
    pub profile: Option<crate::engine::profiles::ProfileRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProviderSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

fn deserialize_provider_section<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ProviderSection>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ProviderField {
        Section(ProviderSection),
        Name(String),
    }

    match Option::<ProviderField>::deserialize(deserializer)? {
        Some(ProviderField::Section(section)) => Ok(Some(section)),
        Some(ProviderField::Name(provider)) => Ok(Some(ProviderSection {
            provider: Some(provider),
            ..Default::default()
        })),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct IgnoreSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HookSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_diff_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_violation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct OutputSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<bool>,
}

/// Review-specific configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ReviewSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
    /// Static analysis context injection (e.g., clippy output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_analysis: Option<StaticAnalysisConfig>,
    /// Context chain configuration (cross-file dependency extraction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_chain: Option<crate::engine::context::types::ContextConfig>,
}

/// Static analysis configuration — inject linter/compiler output as review context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StaticAnalysisConfig {
    /// Automatically run `cargo clippy` and inject output as context.
    /// Adds ~2-5 seconds to review time.
    #[serde(default, skip_serializing_if = "is_default")]
    pub auto_clippy: bool,
    /// Path to a file containing pre-computed static analysis output (e.g., clippy JSON).
    /// If set, this file's content is injected instead of running auto_clippy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clippy_output_file: Option<String>,
}

/// Analysis configuration — controls dead-code detection and symbol search.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AnalysisConfig {
    /// Custom entry-point method patterns to exclude from dead-code detection.
    /// Supports glob-like patterns: `prefix*`, `*suffix`, `*substring*`.
    /// Example: `["*Handler", "resolve_*", "*Middleware"]`
    #[serde(default, skip_serializing_if = "is_default")]
    pub entry_point_patterns: Vec<String>,
}

fn is_default<T: Default + PartialEq>(val: &T) -> bool {
    *val == T::default()
}

/// Scan-specific configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ScanSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_file: Option<String>,
}

/// LLM-specific configuration section (temperature, `max_tokens`, timeout, `cache_ttl`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LlmSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Name of the max tokens parameter to use in API requests.
    /// Supported values: `"auto"` (detect from provider), `"max_tokens"`,
    /// `"max_output_tokens"`, `"max_completion_tokens"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<u64>,
}

fn default_max_findings() -> usize {
    5
}

/// Rule engine configuration section for `.cora.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RulesSection {
    #[serde(default, skip_serializing_if = "is_default")]
    pub enabled: bool,
    #[serde(default = "default_max_findings")]
    pub max_findings: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<crate::engine::rules::types::CustomRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index_skip_files: Vec<String>,
}

/// File bundling configuration section for `.cora.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BundlingSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_chars_per_group: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files_per_group: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<crate::engine::bundling::GroupingStrategy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesce_by_directory: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesce_by_language: Option<bool>,
}

impl CoraFile {
    pub fn from_str(content: &str) -> std::result::Result<Self, CoraError> {
        serde_yaml_ng::from_str(content).map_err(|e| CoraError::ConfigParse(e.to_string()))
    }

    /// Merge this file config into a `Config`, overwriting only fields that are present.
    #[allow(clippy::assigning_clones)]
    pub fn merge_into(&self, config: &mut Config) -> Result<(), CoraError> {
        if let Some(v) = &self.model {
            config.provider.model.clone_from(v);
        }
        if let Some(v) = &self.base_url {
            config.provider.base_url.clone_from(v);
        }
        if let Some(p) = &self.provider {
            if let Some(v) = &p.provider {
                config.provider.provider.clone_from(v);

                // Resolve preset defaults (base_url, model) when provider is a known preset
                // and the corresponding field wasn't explicitly set in the config file.
                if let Some(preset) = crate::config::providers::PRESETS
                    .iter()
                    .find(|pr| pr.name == v)
                {
                    if p.base_url.is_none() && self.base_url.is_none() {
                        config.provider.base_url = preset.default_base_url.to_string();
                    }
                    if p.model.is_none() && self.model.is_none() {
                        config.provider.model = preset.default_model.to_string();
                    }
                }
            }
            if let Some(v) = &p.model {
                config.provider.model.clone_from(v);
            }
            if let Some(v) = &p.base_url {
                config.provider.base_url.clone_from(v);
            }
        }
        if let Some(v) = &self.focus {
            config.focus.clone_from(v);
        }
        if let Some(v) = &self.rules {
            config.rules.clone_from(v);
        }
        if let Some(ig) = &self.ignore {
            if let Some(v) = &ig.files {
                config.ignore.files.clone_from(v);
            }
            if let Some(v) = &ig.rules {
                config.ignore.rules.clone_from(v);
            }
        }
        if let Some(h) = &self.hook {
            if let Some(v) = &h.mode {
                config.hook.mode.clone_from(v);
            }
            if let Some(v) = &h.min_severity {
                config.hook.min_severity.clone_from(v);
            }
            if let Some(v) = h.max_diff_size {
                config.hook.max_diff_size = v;
            }
            if let Some(v) = &h.on_violation {
                config.hook.on_violation.clone_from(v);
            }
        }
        if let Some(qg) = &self.quality_gate {
            config.quality_gate = qg.clone();
        }
        if let Some(o) = &self.output {
            if let Some(v) = &o.format {
                config.output.format.clone_from(v);
            }
            if let Some(v) = o.color {
                config.output.color = v;
            }
        }
        if let Some(r) = &self.review {
            if let Some(v) = &r.response_format {
                config.response_format.clone_from(v);
            }
            if let Some(v) = &r.system_prompt {
                config.review_system_prompt_override = Some(v.clone());
            }
            if let Some(v) = &r.system_prompt_file {
                config.review_system_prompt_file = Some(v.clone());
            }
            if let Some(sa) = &r.static_analysis {
                config.static_analysis.clone_from(sa);
            }
            if let Some(cc) = &r.context_chain {
                config.context_chain.clone_from(cc);
            }
        }
        if let Some(s) = &self.scan {
            if let Some(v) = &s.system_prompt {
                config.scan_system_prompt_override = Some(v.clone());
            }
            if let Some(v) = &s.system_prompt_file {
                config.scan_system_prompt_file = Some(v.clone());
            }
        }
        if let Some(llm) = &self.llm {
            if let Some(v) = llm.temperature {
                config.temperature = v;
            }
            if let Some(v) = llm.max_tokens {
                config.max_tokens = v;
            }
            if let Some(v) = llm.timeout {
                config.timeout = v;
            }
            if let Some(v) = llm.cache_ttl {
                config.cache_ttl = v;
            }
            if let Some(ref v) = llm.max_tokens_param {
                config.max_tokens_param = v.clone();
            }
        }
        if let Some(re) = &self.rules_engine {
            config.rules_config.enabled = re.enabled;
            config.rules_config.max_findings = re.max_findings;
            if !re.custom.is_empty() {
                config.rules_config.custom_rules = re.custom.clone();
            }
            if !re.index_skip_files.is_empty() {
                config.rules_config.index_skip_files = re.index_skip_files.clone();
            }
        }
        if let Some(b) = &self.bundling {
            if let Some(v) = b.max_chars_per_group {
                config.bundling.max_chars_per_group = v;
            }
            if let Some(v) = b.max_files_per_group {
                config.bundling.max_files_per_group = v;
            }
            if let Some(v) = b.strategy {
                config.bundling.strategy = v;
            }
            if let Some(v) = b.coalesce_by_directory {
                config.bundling.coalesce_by_directory = v;
            }
            if let Some(v) = b.coalesce_by_language {
                config.bundling.coalesce_by_language = v;
            }
        }
        // Merge debt tracking config
        if let Some(debt) = &self.debt {
            config.debt.enabled = debt.enabled;
            if debt.history_dir.is_some() {
                config.debt.history_dir.clone_from(&debt.history_dir);
            }
            config.debt.retention_days = debt.retention_days;
        }
        // Resolve profile — load built-in or custom, merge into config
        if let Some(profile_ref) = &self.profile {
            match crate::engine::profiles::resolve_profile(profile_ref) {
                Ok(profile) => {
                    config.profile = Some(profile);
                }
                Err(e) => {
                    // Fail fast — invalid profile means review policy is broken.
                    // Better to error than silently run without the intended policy.
                    return Err(CoraError::ConfigParse(format!(
                        "invalid profile in config: {e}"
                    )));
                }
            }
        }
        // Merge analysis config
        if let Some(analysis) = &self.analysis {
            if !analysis.entry_point_patterns.is_empty() {
                config
                    .analysis
                    .entry_point_patterns
                    .clone_from(&analysis.entry_point_patterns);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    // ─── Config::default() ───

    #[test]
    fn config_default_provider() {
        let cfg = Config::default();
        assert_eq!(cfg.provider.provider, "openai");
        assert_eq!(cfg.provider.model, "gpt-4o-mini");
        assert_eq!(cfg.provider.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn config_default_focus() {
        let cfg = Config::default();
        assert_eq!(
            cfg.focus,
            vec!["security", "performance", "bugs", "best_practice"]
        );
    }

    #[test]
    fn config_default_rules_empty() {
        let cfg = Config::default();
        assert!(cfg.rules.is_empty());
    }

    #[test]
    fn config_default_ignore_files() {
        let cfg = Config::default();
        assert!(cfg.ignore.files.contains(&"node_modules/**".to_string()));
        assert!(cfg.ignore.files.contains(&"dist/**".to_string()));
        assert!(cfg.ignore.files.contains(&"target/**".to_string()));
        assert!(cfg.ignore.files.contains(&".git/**".to_string()));
    }

    #[test]
    fn config_default_hook() {
        let cfg = Config::default();
        assert_eq!(cfg.hook.mode, "warn");
        assert_eq!(cfg.hook.min_severity, "major");
        assert_eq!(cfg.hook.max_diff_size, 5 * 1024 * 1024);
        assert_eq!(cfg.hook.on_violation, "warn");
    }

    #[test]
    fn config_default_output() {
        let cfg = Config::default();
        assert_eq!(cfg.output.format, "pretty");
        assert!(cfg.output.color);
    }

    // ─── CoraFile::merge_into ───

    #[test]
    fn merge_empty_cora_file_leaves_defaults() {
        let mut cfg = Config::default();
        let cora = CoraFile::default();
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.provider.provider, "openai");
        assert_eq!(cfg.provider.model, "gpt-4o-mini");
        assert_eq!(cfg.output.format, "pretty");
    }

    #[test]
    fn merge_provider_overrides() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            provider: Some(ProviderSection {
                provider: Some("anthropic".to_string()),
                model: Some("claude-3-haiku".to_string()),
                base_url: Some("https://api.anthropic.com/v1".to_string()),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.provider.provider, "anthropic");
        assert_eq!(cfg.provider.model, "claude-3-haiku");
        assert_eq!(cfg.provider.base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn merge_top_level_provider_shortcuts() {
        let mut cfg = Config::default();
        let cora = CoraFile::from_str(
            r"
provider: openai
model: glm-5.1
base_url: https://api.z.ai/api/coding/paas/v4
",
        )
        .unwrap();

        cora.merge_into(&mut cfg).unwrap();

        assert_eq!(cfg.provider.provider, "openai");
        assert_eq!(cfg.provider.model, "glm-5.1");
        assert_eq!(cfg.provider.base_url, "https://api.z.ai/api/coding/paas/v4");
    }

    #[test]
    fn merge_partial_provider() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            provider: Some(ProviderSection {
                provider: Some("ollama".to_string()),
                model: None,
                base_url: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.provider.provider, "ollama");
        assert_eq!(cfg.provider.model, "llama3.1"); // resolved from ollama preset
        assert_eq!(cfg.provider.base_url, "http://localhost:11434/v1"); // resolved from ollama preset
    }

    #[test]
    fn merge_shortcut_provider_resolves_preset() {
        let mut cfg = Config::default();
        let cora = CoraFile::from_str(
            r"
provider: zai
",
        )
        .unwrap();

        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.provider.provider, "zai");
        assert_eq!(cfg.provider.model, "glm-5.1"); // resolved from zai preset
        assert_eq!(cfg.provider.base_url, "https://api.z.ai/api/coding/paas/v4"); // resolved from zai preset
    }

    #[test]
    fn merge_focus() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            focus: Some(vec!["security".to_string(), "bugs".to_string()]),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.focus, vec!["security", "bugs"]);
    }

    #[test]
    fn merge_rules() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            rules: Some(vec!["no unwrap".to_string()]),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.rules, vec!["no unwrap"]);
    }

    #[test]
    fn merge_ignore() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            ignore: Some(IgnoreSection {
                files: Some(vec!["vendor/**".to_string()]),
                rules: Some(vec!["skip-rule-1".to_string()]),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.ignore.files, vec!["vendor/**"]);
        assert_eq!(cfg.ignore.rules, vec!["skip-rule-1"]);
    }

    #[test]
    fn merge_hook() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            hook: Some(HookSection {
                mode: Some("block".to_string()),
                min_severity: Some("critical".to_string()),
                max_diff_size: Some(1024),
                on_violation: Some("disallow".to_string()),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.hook.mode, "block");
        assert_eq!(cfg.hook.min_severity, "critical");
        assert_eq!(cfg.hook.max_diff_size, 1024);
        assert_eq!(cfg.hook.on_violation, "disallow");
    }

    #[test]
    fn merge_output() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            output: Some(OutputSection {
                format: Some("json".to_string()),
                color: Some(false),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.output.format, "json");
        assert!(!cfg.output.color);
    }

    // ─── CoraFile::from_str (YAML parsing) ───

    #[test]
    fn parse_cora_file_empty() {
        let cora = CoraFile::from_str("").unwrap();
        assert!(cora.provider.is_none());
        assert!(cora.focus.is_none());
    }

    #[test]
    fn parse_cora_file_full() {
        let yaml = r"
provider:
  provider: anthropic
  model: claude-3-haiku
  base_url: https://api.anthropic.com/v1
focus:
  - security
  - bugs
rules:
  - no unwrap
ignore:
  files:
    - vendor/**
hook:
  mode: block
  min_severity: critical
output:
  format: json
  color: false
";
        let cora = CoraFile::from_str(yaml).unwrap();
        assert_eq!(
            cora.provider.as_ref().unwrap().provider.as_deref(),
            Some("anthropic")
        );
        assert_eq!(cora.focus.as_ref().unwrap().len(), 2);
        assert_eq!(cora.rules.as_ref().unwrap().len(), 1);
        assert_eq!(
            cora.output.as_ref().unwrap().format.as_deref(),
            Some("json")
        );
        assert_eq!(cora.output.as_ref().unwrap().color, Some(false));
    }

    // ─── HookConfig::min_severity_level ───

    #[test]
    fn hook_min_severity_level() {
        let cfg = HookConfig {
            mode: "warn".to_string(),
            min_severity: "critical".to_string(),
            max_diff_size: 1024,
            on_violation: "warn".to_string(),
        };
        assert_eq!(cfg.min_severity_level(), Severity::Critical);
    }

    #[test]
    fn hook_min_severity_level_unknown() {
        let cfg = HookConfig {
            mode: "warn".to_string(),
            min_severity: "whatever".to_string(),
            max_diff_size: 1024,
            on_violation: "warn".to_string(),
        };
        assert_eq!(cfg.min_severity_level(), Severity::Info);
    }

    // ─── CoraFile serde round-trip ───

    #[test]
    fn cora_file_yaml_roundtrip() {
        let cora = CoraFile {
            provider: Some(ProviderSection {
                provider: Some("ollama".to_string()),
                model: Some("llama3".to_string()),
                base_url: Some("http://localhost:11434".to_string()),
            }),
            focus: Some(vec!["security".to_string()]),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&cora).unwrap();
        let back: CoraFile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            back.provider.as_ref().unwrap().provider.as_deref(),
            Some("ollama")
        );
        assert_eq!(back.focus.as_ref().unwrap().len(), 1);
    }

    // ─── Config serde round-trip ───

    #[test]
    fn config_json_roundtrip() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider.provider, cfg.provider.provider);
        assert_eq!(back.output.format, cfg.output.format);
    }

    // ─── Config::default() new fields ───

    #[test]
    fn config_default_response_format_none() {
        let cfg = Config::default();
        assert_eq!(cfg.response_format, "none");
    }

    #[test]
    fn config_default_system_prompt_overrides_none() {
        let cfg = Config::default();
        assert!(cfg.review_system_prompt_override.is_none());
        assert!(cfg.review_system_prompt_file.is_none());
        assert!(cfg.scan_system_prompt_override.is_none());
        assert!(cfg.scan_system_prompt_file.is_none());
    }

    // ─── ReviewSection parsing and merge ───

    #[test]
    fn parse_review_section_with_response_format() {
        let yaml = r"
review:
  response_format: json_object
";
        let cora = CoraFile::from_str(yaml).unwrap();
        assert_eq!(
            cora.review.as_ref().unwrap().response_format.as_deref(),
            Some("json_object")
        );
    }

    #[test]
    fn parse_review_section_with_system_prompt() {
        let yaml = r"
review:
  system_prompt: |
    You are a security-focused reviewer.
  system_prompt_file: .cora/prompts/review.md
";
        let cora = CoraFile::from_str(yaml).unwrap();
        assert_eq!(
            cora.review.as_ref().unwrap().system_prompt.as_deref(),
            Some("You are a security-focused reviewer.\n")
        );
        assert_eq!(
            cora.review.as_ref().unwrap().system_prompt_file.as_deref(),
            Some(".cora/prompts/review.md")
        );
    }

    #[test]
    fn merge_review_response_format() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            review: Some(ReviewSection {
                response_format: Some("json_object".to_string()),
                system_prompt: None,
                system_prompt_file: None,
                static_analysis: None,
                context_chain: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.response_format, "json_object");
    }

    #[test]
    fn merge_review_system_prompt() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            review: Some(ReviewSection {
                response_format: None,
                system_prompt: Some("Custom prompt here.".to_string()),
                system_prompt_file: None,
                static_analysis: None,
                context_chain: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(
            cfg.review_system_prompt_override.as_deref(),
            Some("Custom prompt here.")
        );
    }

    #[test]
    fn merge_review_system_prompt_file() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            review: Some(ReviewSection {
                response_format: None,
                system_prompt: None,
                system_prompt_file: Some("prompts/review.md".to_string()),
                static_analysis: None,
                context_chain: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(
            cfg.review_system_prompt_file.as_deref(),
            Some("prompts/review.md")
        );
    }

    // ─── ScanSection parsing and merge ───

    #[test]
    fn parse_scan_section_with_system_prompt() {
        let yaml = r"
scan:
  system_prompt: |
    You are a performance-focused scanner.
";
        let cora = CoraFile::from_str(yaml).unwrap();
        assert_eq!(
            cora.scan.as_ref().unwrap().system_prompt.as_deref(),
            Some("You are a performance-focused scanner.\n")
        );
    }

    #[test]
    fn merge_scan_system_prompt() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            scan: Some(ScanSection {
                system_prompt: Some("Performance only.".to_string()),
                system_prompt_file: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(
            cfg.scan_system_prompt_override.as_deref(),
            Some("Performance only.")
        );
    }

    // ─── Full .cora.yaml with review and scan sections ───

    #[test]
    fn parse_cora_file_with_review_and_scan() {
        let yaml = r"
review:
  response_format: json_object
  system_prompt: |
    Security only.
scan:
  system_prompt: |
    Performance only.
  system_prompt_file: scan.md
";
        let cora = CoraFile::from_str(yaml).unwrap();
        let review = cora.review.unwrap();
        assert_eq!(review.response_format.as_deref(), Some("json_object"));
        assert_eq!(review.system_prompt.as_deref(), Some("Security only.\n"));
        let scan = cora.scan.unwrap();
        assert_eq!(scan.system_prompt.as_deref(), Some("Performance only.\n"));
        assert_eq!(scan.system_prompt_file.as_deref(), Some("scan.md"));
    }

    // ─── LLM section parsing and merge ───

    #[test]
    fn config_default_temperature_is_zero() {
        let cfg = Config::default();
        assert_eq!(cfg.temperature, 0.0);
    }

    #[test]
    fn config_default_max_tokens() {
        let cfg = Config::default();
        assert_eq!(cfg.max_tokens, 4096);
    }

    #[test]
    fn config_default_timeout() {
        let cfg = Config::default();
        assert_eq!(cfg.timeout, 600);
    }

    #[test]
    fn config_default_cache_ttl() {
        let cfg = Config::default();
        assert_eq!(cfg.cache_ttl, 1440);
    }

    #[test]
    fn parse_llm_section() {
        let yaml = r"
llm:
  temperature: 0.5
  max_tokens: 8192
  timeout: 60
  cache_ttl: 720
";
        let cora = CoraFile::from_str(yaml).unwrap();
        let llm = cora.llm.unwrap();
        assert_eq!(llm.temperature, Some(0.5));
        assert_eq!(llm.max_tokens, Some(8192));
        assert_eq!(llm.timeout, Some(60));
        assert_eq!(llm.cache_ttl, Some(720));
    }

    #[test]
    fn parse_llm_section_partial() {
        let yaml = r"
llm:
  temperature: 0.3
";
        let cora = CoraFile::from_str(yaml).unwrap();
        let llm = cora.llm.unwrap();
        assert_eq!(llm.temperature, Some(0.3));
        assert_eq!(llm.max_tokens, None);
        assert_eq!(llm.timeout, None);
        assert_eq!(llm.cache_ttl, None);
    }

    #[test]
    fn merge_llm_temperature() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: Some(0.7),
                max_tokens: None,
                max_tokens_param: None,
                timeout: None,
                cache_ttl: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.temperature, 0.7);
        // Other LLM fields should remain at defaults
        assert_eq!(cfg.max_tokens, 4096);
        assert_eq!(cfg.timeout, 600);
        assert_eq!(cfg.cache_ttl, 1440);
    }

    #[test]
    fn merge_llm_max_tokens() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: None,
                max_tokens: Some(2048),
                max_tokens_param: None,
                timeout: None,
                cache_ttl: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.max_tokens, 2048);
    }

    #[test]
    fn merge_llm_timeout() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: None,
                max_tokens: None,
                max_tokens_param: None,
                timeout: Some(300),
                cache_ttl: None,
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.timeout, 300);
    }

    #[test]
    fn merge_llm_all_fields() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: Some(1.0),
                max_tokens: Some(16384),
                max_tokens_param: None,
                timeout: Some(240),
                cache_ttl: Some(2880),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.temperature, 1.0);
        assert_eq!(cfg.max_tokens, 16384);
        assert_eq!(cfg.timeout, 240);
        assert_eq!(cfg.cache_ttl, 2880);
    }

    // ─── Config error on malformed YAML ───

    #[test]
    fn cora_file_malformed_yaml_returns_error() {
        let yaml = r"
provider:
  provider: openai
  model: gpt-4
  this is not valid yaml: [
";
        let result = CoraFile::from_str(yaml);
        assert!(result.is_err(), "malformed YAML should return an error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("config parse error"),
            "error message should mention parse failure: {err}"
        );
    }

    #[test]
    fn cora_file_empty_yaml_is_ok() {
        let cora = CoraFile::from_str("").unwrap();
        assert!(cora.llm.is_none());
        assert!(cora.provider.is_none());
    }

    #[test]
    fn cora_file_yaml_roundtrip_with_llm() {
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: Some(0.5),
                max_tokens: Some(8192),
                max_tokens_param: None,
                timeout: Some(60),
                cache_ttl: None,
            }),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&cora).unwrap();
        let back: CoraFile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.llm.as_ref().unwrap().temperature, Some(0.5));
        assert_eq!(back.llm.as_ref().unwrap().max_tokens, Some(8192));
    }

    // ─── BundlingSection parsing and merge ───

    #[test]
    fn config_default_bundling() {
        let cfg = Config::default();
        assert_eq!(cfg.bundling.max_chars_per_group, 60_000);
        assert_eq!(cfg.bundling.max_files_per_group, 20);
        assert_eq!(
            cfg.bundling.strategy,
            crate::engine::bundling::GroupingStrategy::Smart
        );
        assert!(cfg.bundling.coalesce_by_directory);
        assert!(cfg.bundling.coalesce_by_language);
    }

    #[test]
    fn parse_bundling_section_full() {
        let yaml = r"
bundling:
  max_chars_per_group: 30000
  max_files_per_group: 10
  strategy: flat
  coalesce_by_directory: false
  coalesce_by_language: false
";
        let cora = CoraFile::from_str(yaml).unwrap();
        let b = cora.bundling.unwrap();
        assert_eq!(b.max_chars_per_group, Some(30_000));
        assert_eq!(b.max_files_per_group, Some(10));
        assert_eq!(
            b.strategy,
            Some(crate::engine::bundling::GroupingStrategy::Flat)
        );
        assert_eq!(b.coalesce_by_directory, Some(false));
        assert_eq!(b.coalesce_by_language, Some(false));
    }

    #[test]
    fn parse_bundling_section_partial() {
        let yaml = r"
bundling:
  max_chars_per_group: 40000
  strategy: flat
";
        let cora = CoraFile::from_str(yaml).unwrap();
        let b = cora.bundling.unwrap();
        assert_eq!(b.max_chars_per_group, Some(40_000));
        assert_eq!(b.max_files_per_group, None);
        assert_eq!(
            b.strategy,
            Some(crate::engine::bundling::GroupingStrategy::Flat)
        );
        assert_eq!(b.coalesce_by_directory, None);
        assert_eq!(b.coalesce_by_language, None);
    }

    #[test]
    fn merge_bundling_all_fields() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            bundling: Some(BundlingSection {
                max_chars_per_group: Some(30_000),
                max_files_per_group: Some(10),
                strategy: Some(crate::engine::bundling::GroupingStrategy::Flat),
                coalesce_by_directory: Some(false),
                coalesce_by_language: Some(false),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.bundling.max_chars_per_group, 30_000);
        assert_eq!(cfg.bundling.max_files_per_group, 10);
        assert_eq!(
            cfg.bundling.strategy,
            crate::engine::bundling::GroupingStrategy::Flat
        );
        assert!(!cfg.bundling.coalesce_by_directory);
        assert!(!cfg.bundling.coalesce_by_language);
    }

    #[test]
    fn merge_bundling_partial() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            bundling: Some(BundlingSection {
                max_chars_per_group: Some(40_000),
                max_files_per_group: None,
                strategy: None,
                coalesce_by_directory: None,
                coalesce_by_language: Some(false),
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.bundling.max_chars_per_group, 40_000);
        assert_eq!(cfg.bundling.max_files_per_group, 20); // default unchanged
        assert_eq!(
            cfg.bundling.strategy,
            crate::engine::bundling::GroupingStrategy::Smart
        ); // default unchanged
        assert!(cfg.bundling.coalesce_by_directory); // default unchanged
        assert!(!cfg.bundling.coalesce_by_language);
    }

    #[test]
    fn merge_bundling_absent_leaves_defaults() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.bundling.max_chars_per_group, 60_000);
        assert_eq!(cfg.bundling.max_files_per_group, 20);
    }

    #[test]
    fn bundling_section_yaml_roundtrip() {
        let section = BundlingSection {
            max_chars_per_group: Some(50_000),
            max_files_per_group: Some(15),
            strategy: Some(crate::engine::bundling::GroupingStrategy::Smart),
            coalesce_by_directory: Some(true),
            coalesce_by_language: Some(false),
        };
        let yaml = serde_yaml_ng::to_string(&section).unwrap();
        let back: BundlingSection = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.max_chars_per_group, Some(50_000));
        assert_eq!(back.max_files_per_group, Some(15));
        assert_eq!(
            back.strategy,
            Some(crate::engine::bundling::GroupingStrategy::Smart)
        );
    }

    // ─── max_tokens_param ───

    #[test]
    fn config_default_max_tokens_param() {
        let cfg = Config::default();
        assert_eq!(cfg.max_tokens_param, "auto");
    }

    #[test]
    fn merge_llm_max_tokens_param_explicit() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                max_tokens_param: Some("max_output_tokens".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.max_tokens_param, "max_output_tokens");
    }

    #[test]
    fn merge_llm_max_tokens_param_absent_leaves_default() {
        let mut cfg = Config::default();
        let cora = CoraFile {
            llm: Some(LlmSection {
                temperature: Some(0.5),
                ..Default::default()
            }),
            ..Default::default()
        };
        cora.merge_into(&mut cfg).unwrap();
        assert_eq!(cfg.max_tokens_param, "auto");
    }

    #[test]
    fn llm_section_yaml_roundtrip_with_max_tokens_param() {
        let section = LlmSection {
            temperature: Some(0.7),
            max_tokens: Some(8192),
            max_tokens_param: Some("max_output_tokens".to_string()),
            timeout: Some(300),
            cache_ttl: Some(60),
        };
        let yaml = serde_yaml_ng::to_string(&section).unwrap();
        let back: LlmSection = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.max_tokens_param, Some("max_output_tokens".to_string()));
        assert_eq!(back.max_tokens, Some(8192));
    }

    // ─── #94: Config::validate ───

    #[test]
    fn validate_accepts_default_config() {
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_temperature() {
        let cfg = Config {
            temperature: 5.0,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("temperature"), "err: {err}");
    }

    #[test]
    fn validate_rejects_invalid_output_format() {
        let mut cfg = Config::default();
        cfg.output.format = "prety".to_string(); // typo
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("output.format"), "err: {err}");
    }

    #[test]
    fn validate_rejects_invalid_max_tokens_param() {
        let cfg = Config {
            max_tokens_param: "tokens".to_string(),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_base_url() {
        let mut cfg = Config::default();
        cfg.provider.base_url = "api.openai.com".to_string(); // missing scheme
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("base_url"), "err: {err}");
    }

    #[test]
    fn validate_aggregates_multiple_errors() {
        let cfg = Config {
            temperature: 9.0,
            timeout: 0,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("temperature"), "err: {err}");
        assert!(err.contains("timeout"), "err: {err}");
    }

    // ─── #80: deny_unknown_fields ───

    #[test]
    fn deny_unknown_fields_rejects_top_level_typo() {
        let yaml = "\nquailty_gate:\n  enabled: true\n";
        let result = CoraFile::from_str(yaml);
        assert!(result.is_err(), "misspelled top-level key must be rejected");
    }

    #[test]
    fn deny_unknown_fields_rejects_section_typo() {
        let yaml = "\nllm:\n  temprature: 0.5\n";
        let result = CoraFile::from_str(yaml);
        assert!(result.is_err(), "misspelled section key must be rejected");
    }

    #[test]
    fn deny_unknown_fields_allows_valid_keys() {
        let yaml = "\nllm:\n  temperature: 0.5\n  max_tokens: 8192\n";
        assert!(CoraFile::from_str(yaml).is_ok());
    }
}
