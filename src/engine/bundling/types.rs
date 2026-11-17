/// Bundling types — configuration and data structures for file grouping.
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

/// A group of files to be reviewed together in a single LLM call.
///
/// Each group is a coherent unit — ideally files that are related by
/// directory, language, or feature area — kept within character and file
/// count limits.
#[derive(Debug, Clone)]
pub struct FileGroup {
    /// Files in this group.
    pub files: Vec<crate::engine::scanner::FileEntry>,
    /// Total character count (content + path headers + line numbers).
    pub total_chars: usize,
}

impl FileGroup {
    /// Create a new empty group.
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            total_chars: 0,
        }
    }

    /// Estimated character size for a single file entry (content + header overhead).
    pub fn file_cost(file: &crate::engine::scanner::FileEntry) -> usize {
        file.content.len() + file.path.len() + 20 // header overhead
    }

    /// Check if adding a file would exceed the limits.
    pub fn would_fit(
        &self,
        file: &crate::engine::scanner::FileEntry,
        max_chars: usize,
        max_files: usize,
    ) -> bool {
        if self.files.len() >= max_files {
            return false;
        }
        let cost = Self::file_cost(file);
        // Allow first file even if it exceeds max_chars (single large file)
        if self.files.is_empty() {
            return true;
        }
        self.total_chars + cost <= max_chars
    }

    /// Add a file to the group.
    pub fn push(&mut self, file: crate::engine::scanner::FileEntry) {
        self.total_chars += Self::file_cost(&file);
        self.files.push(file);
    }

    /// Number of files in the group.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the group is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Language family classification for grouping.
///
/// Files with extensions in the same family are preferred to be grouped together,
/// as they often belong to the same project layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageFamily {
    /// Rust (rs)
    Rust,
    /// JavaScript/TypeScript (js, ts, tsx, jsx, mjs, cjs)
    JavaScript,
    /// Python (py, pyi)
    Python,
    /// Go (go, mod, sum)
    Go,
    /// Java/JVM (java, kt, scala, gradle)
    Jvm,
    /// Web (html, css, scss, less, vue, svelte)
    Web,
    /// C/C++ (c, cpp, h, hpp, cc, cxx)
    CFamily,
    /// Shell (sh, bash, zsh, ps1)
    Shell,
    /// Config (yaml, yml, json, toml, ini)
    Config,
    /// Documentation (md, rst, txt, adoc)
    Documentation,
    /// Database (sql, graphql, proto)
    Database,
    /// Other / unclassified
    Other,
}

impl LanguageFamily {
    /// Classify a file extension into a language family.
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" => LanguageFamily::Rust,
            "js" | "ts" | "tsx" | "jsx" | "mjs" | "cjs" => LanguageFamily::JavaScript,
            "py" | "pyi" => LanguageFamily::Python,
            "go" | "mod" | "sum" => LanguageFamily::Go,
            "java" | "kt" | "scala" | "gradle" => LanguageFamily::Jvm,
            "html" | "css" | "scss" | "less" | "vue" | "svelte" => LanguageFamily::Web,
            "c" | "cpp" | "h" | "hpp" | "cc" | "cxx" => LanguageFamily::CFamily,
            "sh" | "bash" | "zsh" | "ps1" => LanguageFamily::Shell,
            "yaml" | "yml" | "json" | "toml" | "ini" => LanguageFamily::Config,
            "md" | "rst" | "txt" | "adoc" => LanguageFamily::Documentation,
            "sql" | "graphql" | "proto" => LanguageFamily::Database,
            _ => LanguageFamily::Other,
        }
    }
}

impl std::fmt::Display for LanguageFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LanguageFamily::Rust => write!(f, "rust"),
            LanguageFamily::JavaScript => write!(f, "javascript"),
            LanguageFamily::Python => write!(f, "python"),
            LanguageFamily::Go => write!(f, "go"),
            LanguageFamily::Jvm => write!(f, "jvm"),
            LanguageFamily::Web => write!(f, "web"),
            LanguageFamily::CFamily => write!(f, "c_family"),
            LanguageFamily::Shell => write!(f, "shell"),
            LanguageFamily::Config => write!(f, "config"),
            LanguageFamily::Documentation => write!(f, "documentation"),
            LanguageFamily::Database => write!(f, "database"),
            LanguageFamily::Other => write!(f, "other"),
        }
    }
}

/// Key used for grouping files together.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupingKey {
    /// The parent directory of the file (relative to project root).
    pub directory: String,
    /// The language family of the file.
    pub language: LanguageFamily,
}

impl GroupingKey {
    /// Create a grouping key from a file path and extension.
    pub fn from_file(path: &str, extension: Option<&str>) -> Self {
        let directory = path
            .rsplit_once('/')
            .or_else(|| path.rsplit_once('\\'))
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| ".".to_string());

        let language = extension
            .map(LanguageFamily::from_extension)
            .unwrap_or(LanguageFamily::Other);

        Self {
            directory,
            language,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── BundlingConfig defaults ───

    #[test]
    fn bundling_config_default() {
        let cfg = BundlingConfig::default();
        assert_eq!(cfg.max_chars_per_group, 60_000);
        assert_eq!(cfg.max_files_per_group, 20);
        assert_eq!(cfg.strategy, GroupingStrategy::Smart);
        assert!(cfg.coalesce_by_directory);
        assert!(cfg.coalesce_by_language);
    }

    // ─── GroupingStrategy display ───

    #[test]
    fn grouping_strategy_display() {
        assert_eq!(format!("{}", GroupingStrategy::Smart), "smart");
        assert_eq!(format!("{}", GroupingStrategy::Flat), "flat");
    }

    // ─── LanguageFamily classification ───

    #[test]
    fn language_family_rust() {
        assert_eq!(LanguageFamily::from_extension("rs"), LanguageFamily::Rust);
        assert_eq!(
            LanguageFamily::from_extension("toml"),
            LanguageFamily::Config
        );
    }

    #[test]
    fn language_family_javascript() {
        assert_eq!(
            LanguageFamily::from_extension("js"),
            LanguageFamily::JavaScript
        );
        assert_eq!(
            LanguageFamily::from_extension("ts"),
            LanguageFamily::JavaScript
        );
        assert_eq!(
            LanguageFamily::from_extension("tsx"),
            LanguageFamily::JavaScript
        );
        assert_eq!(
            LanguageFamily::from_extension("jsx"),
            LanguageFamily::JavaScript
        );
    }

    #[test]
    fn language_family_python() {
        assert_eq!(LanguageFamily::from_extension("py"), LanguageFamily::Python);
    }

    #[test]
    fn language_family_other() {
        assert_eq!(LanguageFamily::from_extension("xyz"), LanguageFamily::Other);
    }

    #[test]
    fn language_family_case_insensitive() {
        assert_eq!(LanguageFamily::from_extension("RS"), LanguageFamily::Rust);
        assert_eq!(LanguageFamily::from_extension("Py"), LanguageFamily::Python);
    }

    // ─── GroupingKey ───

    #[test]
    fn grouping_key_from_file() {
        let key = GroupingKey::from_file("src/engine/mod.rs", Some("rs"));
        assert_eq!(key.directory, "src/engine");
        assert_eq!(key.language, LanguageFamily::Rust);
    }

    #[test]
    fn grouping_key_root_file() {
        let key = GroupingKey::from_file("main.rs", Some("rs"));
        assert_eq!(key.directory, ".");
        assert_eq!(key.language, LanguageFamily::Rust);
    }

    #[test]
    fn grouping_key_no_extension() {
        let key = GroupingKey::from_file("Makefile", None);
        assert_eq!(key.directory, ".");
        assert_eq!(key.language, LanguageFamily::Other);
    }

    // ─── FileGroup ───

    #[test]
    fn file_group_new() {
        let group = FileGroup::new();
        assert!(group.is_empty());
        assert_eq!(group.len(), 0);
        assert_eq!(group.total_chars, 0);
    }

    #[test]
    fn file_group_push_and_fit() {
        let mut group = FileGroup::new();
        let file = crate::engine::scanner::FileEntry {
            path: "test.rs".to_string(),
            content: "fn main() {}".to_string(),
            lines: 1,
        };

        assert!(group.would_fit(&file, 1000, 10));
        group.push(file);
        assert_eq!(group.len(), 1);
        assert!(!group.is_empty());
    }

    #[test]
    fn file_group_max_files_limit() {
        let mut group = FileGroup::new();
        for i in 0..3 {
            group.push(crate::engine::scanner::FileEntry {
                path: format!("file_{i}.rs"),
                content: "x".to_string(),
                lines: 1,
            });
        }
        // 3 files already, max 3
        let new_file = crate::engine::scanner::FileEntry {
            path: "file_4.rs".to_string(),
            content: "x".to_string(),
            lines: 1,
        };
        assert!(!group.would_fit(&new_file, 1_000_000, 3));
    }

    #[test]
    fn file_group_max_chars_limit() {
        let mut group = FileGroup::new();
        group.push(crate::engine::scanner::FileEntry {
            path: "big.rs".to_string(),
            content: "x".repeat(500),
            lines: 1,
        });
        let new_file = crate::engine::scanner::FileEntry {
            path: "small.rs".to_string(),
            content: "x".repeat(500),
            lines: 1,
        };
        // First file always fits, second should not if we set tight limit
        // Total would be ~500 + path + 20 + 500 + path + 20 ≈ 1060
        assert!(!group.would_fit(&new_file, 800, 100));
    }

    #[test]
    fn file_group_first_file_always_fits() {
        let group = FileGroup::new();
        let huge_file = crate::engine::scanner::FileEntry {
            path: "huge.rs".to_string(),
            content: "x".repeat(1_000_000),
            lines: 1000,
        };
        // Even with tiny char limit, first file should fit
        assert!(group.would_fit(&huge_file, 100, 10));
    }

    #[test]
    fn file_cost_calculation() {
        let file = crate::engine::scanner::FileEntry {
            path: "test.rs".to_string(),
            content: "hello world".to_string(),
            lines: 1,
        };
        let cost = FileGroup::file_cost(&file);
        // "hello world".len() = 11 + "test.rs".len() = 7 + 20 overhead = 38
        assert_eq!(cost, 38);
    }

    // ─── GroupingKey equality ───

    #[test]
    fn grouping_key_equality() {
        let k1 = GroupingKey::from_file("src/main.rs", Some("rs"));
        let k2 = GroupingKey::from_file("src/lib.rs", Some("rs"));
        let k3 = GroupingKey::from_file("src/main.rs", Some("py"));

        assert_eq!(k1, k2); // same directory + language
        assert_ne!(k1, k3); // same directory, different language
    }

    // ─── BundlingConfig serde round-trip ───

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
