//! The `bundling:` section of `.cora.yaml`.
//!
//! The grouping engine these keys once configured was never called by review or
//! scan and is gone (#792); the keys are still parsed so an existing `.cora.yaml`
//! keeps loading, and they have no effect.
use serde::{Deserialize, Serialize};

/// Runtime configuration for the file bundling engine (lives on `Config`).
///
/// Controls how files are grouped before being sent to the LLM for review.
/// Smaller, cohesive groups give better review context than arbitrary batching
/// by character count alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlingConfig {
    /// Maximum number of characters per group (token budget).
    /// Files are grouped until this limit is approached.
    pub max_chars_per_group: usize,
    /// Maximum number of files per group.
    pub max_files_per_group: usize,
    /// Preferred grouping strategy.
    ///
    /// - `"smart"` (default): group by directory, then by language, respecting
    ///   character limits. Keeps related files together.
    /// - `"flat"`: simple first-fit batching by character count (legacy behavior,
    ///   equivalent to the old `batch_files` function).
    #[serde(default = "default_grouping_strategy")]
    pub strategy: GroupingStrategy,
    /// Whether to keep files from the same directory together.
    /// When `true`, files in the same directory are prioritized for the same group.
    /// When `false`, groups may mix files from different directories.
    #[serde(default = "default_true")]
    pub coalesce_by_directory: bool,
    /// Whether to group files by language (extension family).
    /// When `true`, files with similar extensions (e.g., `.rs` + `.toml`) are
    /// preferred for the same group.
    #[serde(default = "default_true")]
    pub coalesce_by_language: bool,
}

impl Default for BundlingConfig {
    fn default() -> Self {
        Self {
            max_chars_per_group: 60_000,
            max_files_per_group: 20,
            strategy: default_grouping_strategy(),
            coalesce_by_directory: true,
            coalesce_by_language: true,
        }
    }
}

fn default_grouping_strategy() -> GroupingStrategy {
    GroupingStrategy::Smart
}

fn default_true() -> bool {
    true
}

/// Grouping strategy for file bundling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupingStrategy {
    /// Smart grouping: coalesce by directory and language within limits.
    Smart,
    /// Flat batching: first-fit by character count (legacy).
    Flat,
}

impl std::fmt::Display for GroupingStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupingStrategy::Smart => write!(f, "smart"),
            GroupingStrategy::Flat => write!(f, "flat"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundling_config_default() {
        let cfg = BundlingConfig::default();
        assert_eq!(cfg.max_chars_per_group, 60_000);
        assert_eq!(cfg.max_files_per_group, 20);
        assert_eq!(cfg.strategy, GroupingStrategy::Smart);
        assert!(cfg.coalesce_by_directory);
        assert!(cfg.coalesce_by_language);
    }

    #[test]
    fn grouping_strategy_display() {
        assert_eq!(format!("{}", GroupingStrategy::Smart), "smart");
        assert_eq!(format!("{}", GroupingStrategy::Flat), "flat");
    }

    #[test]
    fn bundling_config_serde_roundtrip() {
        let cfg = BundlingConfig {
            max_chars_per_group: 30_000,
            max_files_per_group: 10,
            strategy: GroupingStrategy::Flat,
            coalesce_by_directory: false,
            coalesce_by_language: false,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: BundlingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_chars_per_group, 30_000);
        assert_eq!(back.max_files_per_group, 10);
        assert_eq!(back.strategy, GroupingStrategy::Flat);
        assert!(!back.coalesce_by_directory);
        assert!(!back.coalesce_by_language);
    }
}
