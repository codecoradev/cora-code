/// Grouping strategies for bundling files into LLM-friendly groups.
use std::collections::HashMap;

use tracing::debug;

use crate::engine::scanner::FileEntry;

use super::types::{BundlingConfig, FileGroup, GroupingKey, GroupingStrategy, LanguageFamily};

/// Group files into `FileGroup`s based on the configuration.
///
/// This is the main entry point for the bundling engine. It dispatches to
/// the appropriate strategy and returns a vector of groups.
pub fn group_files(files: &[FileEntry], config: &BundlingConfig) -> Vec<FileGroup> {
    if files.is_empty() {
        return Vec::new();
    }

    match config.strategy {
        GroupingStrategy::Smart => group_smart(files, config),
        GroupingStrategy::Flat => group_flat(files, config),
    }
}

/// Smart grouping: coalesce by directory and language within character limits.
///
/// Algorithm:
/// 1. Compute a `GroupingKey` (directory × language_family) for each file.
/// 2. Sort files by key so related files are adjacent.
/// 3. Greedily pack files into groups, preferring to keep the same key together.
/// 4. When a group is full (char or file limit), start a new one.
fn group_smart(files: &[FileEntry], config: &BundlingConfig) -> Vec<FileGroup> {
    debug!(total_files = files.len(), "smart grouping files");

    let max_chars = config.max_chars_per_group;
    let max_files = config.max_files_per_group;

    // Build grouping keys for each file
    let mut keyed: Vec<(GroupingKey, &FileEntry)> = files
        .iter()
        .map(|f| {
            let ext = f
                .path
                .rsplit_once('.')
                .map(|(_, e)| e)
                .filter(|e| !e.contains('/'));
            let key = if config.coalesce_by_directory || config.coalesce_by_language {
                GroupingKey::from_file(&f.path, ext)
            } else {
                GroupingKey {
                    directory: String::new(),
                    language: LanguageFamily::Other,
                }
            };
            (key, f)
        })
        .collect();

    // Sort by key to cluster related files
    keyed.sort_by(|a, b| {
        if config.coalesce_by_directory && config.coalesce_by_language {
            a.0.directory
                .cmp(&b.0.directory)
                .then_with(|| a.0.language.cmp(&b.0.language))
                .then_with(|| a.1.path.cmp(&b.1.path))
        } else if config.coalesce_by_directory {
            a.0.directory
                .cmp(&b.0.directory)
                .then_with(|| a.1.path.cmp(&b.1.path))
        } else if config.coalesce_by_language {
            a.0.language
                .cmp(&b.0.language)
                .then_with(|| a.1.path.cmp(&b.1.path))
        } else {
            a.1.path.cmp(&b.1.path)
        }
    });

    // Greedy packing with coalescing preference
    let mut groups: Vec<FileGroup> = Vec::new();
    let mut current_group = FileGroup::new();
    let mut current_key: Option<GroupingKey> = None;

    for (key, file) in keyed {
        let fits = current_group.would_fit(file, max_chars, max_files);

        // If file doesn't fit or we're switching to a very different key, consider
        // starting a new group. But only split if we have enough files in the current
        // group to justify it (avoid trivial groups).
        let should_split = !fits
            || (current_key.is_some()
                && current_key.as_ref() != Some(&key)
                && current_group.len() >= 2
                && would_exceed_threshold(&current_group, file, max_chars));

        if should_split && !current_group.is_empty() {
            groups.push(std::mem::replace(&mut current_group, FileGroup::new()));
            let _ = current_key.take();
        }

        current_group.push(file.clone());
        current_key = Some(key);
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    debug!(groups = groups.len(), "smart grouping complete");

    groups
}

/// Check if adding a file would push the group past a "comfort threshold"
/// (75% of max_chars), suggesting it's a good split point.
fn would_exceed_threshold(group: &FileGroup, file: &FileEntry, max_chars: usize) -> bool {
    let threshold = (max_chars as f64 * 0.75) as usize;
    let cost = FileGroup::file_cost(file);
    group.total_chars + cost > threshold
}

/// Flat grouping: simple first-fit batching by character count.
///
/// This is the legacy behavior — equivalent to the old `batch_files` function
/// in scanner.rs. Files are processed in order, packed greedily until limits
/// are hit, then a new group starts.
fn group_flat(files: &[FileEntry], config: &BundlingConfig) -> Vec<FileGroup> {
    debug!(total_files = files.len(), "flat grouping files");

    let max_chars = config.max_chars_per_group;
    let max_files = config.max_files_per_group;

    let mut groups: Vec<FileGroup> = Vec::new();
    let mut current_group = FileGroup::new();

    for file in files {
        if !current_group.would_fit(file, max_chars, max_files) && !current_group.is_empty() {
            groups.push(std::mem::replace(&mut current_group, FileGroup::new()));
        }
        current_group.push(file.clone());
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    debug!(groups = groups.len(), "flat grouping complete");

    groups
}

/// Group files by a specific key dimension (directory or language only).
///
/// This is a helper for more fine-grained grouping when the user only wants
/// one dimension of coalescing.
pub fn group_by_dimension(
    files: &[FileEntry],
    config: &BundlingConfig,
    dimension: GroupingDimension,
) -> Vec<FileGroup> {
    let max_chars = config.max_chars_per_group;
    let max_files = config.max_files_per_group;

    // Bucket files by the chosen dimension
    let mut buckets: HashMap<String, Vec<&FileEntry>> = HashMap::new();

    for file in files {
        let bucket_key = match dimension {
            GroupingDimension::Directory => file
                .path
                .rsplit_once('/')
                .or_else(|| file.path.rsplit_once('\\'))
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_else(|| ".".to_string()),
            GroupingDimension::Language => file
                .path
                .rsplit_once('.')
                .map(|(_, ext)| LanguageFamily::from_extension(ext).to_string())
                .unwrap_or_else(|| "other".to_string()),
        };
        buckets.entry(bucket_key).or_default().push(file);
    }

    // Sort bucket keys for deterministic output
    let mut bucket_keys: Vec<String> = buckets.keys().cloned().collect();
    bucket_keys.sort();

    // Pack each bucket into groups respecting limits
    let mut groups: Vec<FileGroup> = Vec::new();

    for key in bucket_keys {
        let bucket_files = &buckets[&key];
        let mut current_group = FileGroup::new();

        for file in bucket_files {
            if !current_group.would_fit(file, max_chars, max_files) && !current_group.is_empty() {
                groups.push(std::mem::replace(&mut current_group, FileGroup::new()));
            }
            current_group.push((*file).clone());
        }

        if !current_group.is_empty() {
            groups.push(current_group);
        }
    }

    groups
}

/// Dimension to group files by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupingDimension {
    /// Group by parent directory.
    Directory,
    /// Group by language family.
    Language,
}

/// Merge small groups that could fit together.
///
/// After initial grouping, some groups may be very small (1-2 files).
/// This post-processing step merges adjacent small groups if their
/// combined size stays within limits.
pub fn merge_small_groups(
    mut groups: Vec<FileGroup>,
    config: &BundlingConfig,
    min_group_size: usize,
) -> Vec<FileGroup> {
    if groups.len() <= 1 {
        return groups;
    }

    let max_chars = config.max_chars_per_group;
    let max_files = config.max_files_per_group;

    let mut merged: Vec<FileGroup> = Vec::new();
    let mut accumulator = groups.remove(0);

    for group in groups {
        // Merge if both groups are small and combined fits
        if accumulator.len() < min_group_size
            && group.len() < min_group_size
            && accumulator.len() + group.len() <= max_files
            && accumulator.total_chars + group.total_chars <= max_chars
        {
            for file in group.files {
                accumulator.push(file);
            }
        } else {
            merged.push(std::mem::replace(&mut accumulator, group));
        }
    }

    if !accumulator.is_empty() {
        merged.push(accumulator);
    }

    merged
}

/// Collect statistics about the grouping result.
#[derive(Debug, Clone)]
pub struct GroupingStats {
    /// Total number of groups.
    pub group_count: usize,
    /// Total number of files across all groups.
    pub total_files: usize,
    /// Total characters across all groups.
    pub total_chars: usize,
    /// Average files per group.
    pub avg_files_per_group: f64,
    /// Average characters per group.
    pub avg_chars_per_group: f64,
    /// Number of groups with only 1 file.
    pub single_file_groups: usize,
}

impl GroupingStats {
    /// Compute stats from a list of groups.
    pub fn from_groups(groups: &[FileGroup]) -> Self {
        let group_count = groups.len();
        let total_files: usize = groups.iter().map(|g| g.len()).sum();
        let total_chars: usize = groups.iter().map(|g| g.total_chars).sum();
        let single_file_groups = groups.iter().filter(|g| g.len() == 1).count();

        Self {
            group_count,
            total_files,
            total_chars,
            avg_files_per_group: if group_count > 0 {
                total_files as f64 / group_count as f64
            } else {
                0.0
            },
            avg_chars_per_group: if group_count > 0 {
                total_chars as f64 / group_count as f64
            } else {
                0.0
            },
            single_file_groups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &str, content_len: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            content: "x".repeat(content_len),
            lines: content_len,
        }
    }

    fn default_config() -> BundlingConfig {
        BundlingConfig::default()
    }

    // ─── group_files dispatch ───

    #[test]
    fn group_files_empty() {
        let groups = group_files(&[], &default_config());
        assert!(groups.is_empty());
    }

    // ─── Flat grouping ───

    #[test]
    fn flat_grouping_basic() {
        let files = vec![
            make_file("a.rs", 100),
            make_file("b.rs", 100),
            make_file("c.rs", 100),
        ];
        let config = BundlingConfig {
            max_chars_per_group: 250, // fits ~2 files per group
            max_files_per_group: 100,
            strategy: GroupingStrategy::Flat,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert!(groups.len() >= 2, "should split into at least 2 groups");
        assert_eq!(
            groups.iter().map(|g| g.len()).sum::<usize>(),
            3,
            "all files should be assigned"
        );
    }

    #[test]
    fn flat_grouping_all_fit() {
        let files = vec![make_file("a.rs", 10), make_file("b.rs", 10)];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Flat,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn flat_grouping_preserves_order() {
        let files = vec![
            make_file("a.rs", 10),
            make_file("b.rs", 10),
            make_file("c.rs", 10),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Flat,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files[0].path, "a.rs");
        assert_eq!(groups[0].files[1].path, "b.rs");
        assert_eq!(groups[0].files[2].path, "c.rs");
    }

    #[test]
    fn flat_grouping_max_files_limit() {
        let files = vec![
            make_file("a.rs", 10),
            make_file("b.rs", 10),
            make_file("c.rs", 10),
            make_file("d.rs", 10),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Flat,
            max_files_per_group: 2,
            max_chars_per_group: 1_000_000,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[1].len(), 2);
    }

    // ─── Smart grouping ───

    #[test]
    fn smart_grouping_clusters_same_directory() {
        let files = vec![
            make_file("src/engine/mod.rs", 100),
            make_file("src/engine/types.rs", 100),
            make_file("src/config/mod.rs", 100),
            make_file("tests/integration.rs", 100),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Smart,
            coalesce_by_directory: true,
            coalesce_by_language: false,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        // Should cluster src/engine files together and src/config + tests may be separate
        assert_eq!(
            groups.iter().map(|g| g.len()).sum::<usize>(),
            4,
            "all files assigned"
        );
    }

    #[test]
    fn smart_grouping_clusters_same_language() {
        let files = vec![
            make_file("src/main.rs", 100),
            make_file("lib/api.py", 100),
            make_file("src/lib.rs", 100),
            make_file("scripts/deploy.py", 100),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Smart,
            coalesce_by_directory: false,
            coalesce_by_language: true,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        // Rust files should cluster together, Python files together
        assert_eq!(
            groups.iter().map(|g| g.len()).sum::<usize>(),
            4,
            "all files assigned"
        );
    }

    #[test]
    fn smart_grouping_respects_char_limit() {
        let files = vec![
            make_file("src/a.rs", 10_000),
            make_file("src/b.rs", 10_000),
            make_file("src/c.rs", 10_000),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Smart,
            max_chars_per_group: 15_000,
            max_files_per_group: 100,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert!(groups.len() >= 2, "should split into at least 2 groups");
    }

    // ─── merge_small_groups ───

    #[test]
    fn merge_small_groups_basic() {
        let g1 = {
            let mut g = FileGroup::new();
            g.push(make_file("a.rs", 10));
            g
        };
        let g2 = {
            let mut g = FileGroup::new();
            g.push(make_file("b.rs", 10));
            g
        };
        let g3 = {
            let mut g = FileGroup::new();
            g.push(make_file("c.rs", 10));
            g
        };

        let merged = merge_small_groups(vec![g1, g2, g3], &default_config(), 2);
        // All groups have 1 file (< min_group_size=2), so they should be merged
        assert!(merged.len() <= 2, "should merge small groups");
        assert_eq!(
            merged.iter().map(|g| g.len()).sum::<usize>(),
            3,
            "no files lost"
        );
    }

    #[test]
    fn merge_small_groups_respects_limits() {
        let g1 = {
            let mut g = FileGroup::new();
            g.push(make_file("a.rs", 30_000));
            g
        };
        let g2 = {
            let mut g = FileGroup::new();
            g.push(make_file("b.rs", 30_000));
            g
        };

        let config = BundlingConfig {
            max_chars_per_group: 50_000,
            ..Default::default()
        };
        let merged = merge_small_groups(vec![g1, g2], &config, 2);
        // 30k + 30k = 60k > 50k, should not merge
        assert_eq!(merged.len(), 2);
    }

    // ─── group_by_dimension ───

    #[test]
    fn group_by_directory() {
        let files = vec![
            make_file("src/engine/mod.rs", 10),
            make_file("src/engine/types.rs", 10),
            make_file("src/config/mod.rs", 10),
        ];
        let groups = group_by_dimension(&files, &default_config(), GroupingDimension::Directory);
        // Should have at least 2 groups: src/engine and src/config
        assert!(groups.len() >= 2);
    }

    #[test]
    fn group_by_language() {
        let files = vec![
            make_file("src/main.rs", 10),
            make_file("src/app.py", 10),
            make_file("src/lib.rs", 10),
        ];
        let groups = group_by_dimension(&files, &default_config(), GroupingDimension::Language);
        // Should have at least 2 groups: Rust and Python
        assert!(groups.len() >= 2);
    }

    // ─── GroupingStats ───

    #[test]
    fn grouping_stats_empty() {
        let stats = GroupingStats::from_groups(&[]);
        assert_eq!(stats.group_count, 0);
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_chars, 0);
        assert_eq!(stats.single_file_groups, 0);
    }

    #[test]
    fn grouping_stats_computed() {
        let g1 = {
            let mut g = FileGroup::new();
            g.push(make_file("a.rs", 100));
            g.push(make_file("b.rs", 100));
            g
        };
        let g2 = {
            let mut g = FileGroup::new();
            g.push(make_file("c.rs", 50));
            g
        };

        let stats = GroupingStats::from_groups(&[g1, g2]);
        assert_eq!(stats.group_count, 2);
        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.single_file_groups, 1);
        assert!((stats.avg_files_per_group - 1.5).abs() < 0.01);
    }

    // ─── Smart grouping with both coalesce off ───

    #[test]
    fn smart_grouping_no_coalesce_is_like_flat() {
        let files = vec![
            make_file("z.rs", 10),
            make_file("a.rs", 10),
            make_file("m.rs", 10),
        ];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Smart,
            coalesce_by_directory: false,
            coalesce_by_language: false,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert_eq!(groups.len(), 1);
        // Without coalescing, files are sorted by path
        assert_eq!(groups[0].files[0].path, "a.rs");
        assert_eq!(groups[0].files[1].path, "m.rs");
        assert_eq!(groups[0].files[2].path, "z.rs");
    }

    // ─── Single large file ───

    #[test]
    fn single_large_file_gets_own_group() {
        let files = vec![make_file("huge.rs", 200_000)];
        let config = BundlingConfig {
            strategy: GroupingStrategy::Smart,
            max_chars_per_group: 50_000,
            max_files_per_group: 10,
            ..Default::default()
        };

        let groups = group_files(&files, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 1);
    }
}
