/// File bundling engine — groups files into cohesive units for LLM review.
///
/// Instead of arbitrary batching by character count alone, the bundling engine
/// groups related files together (by directory, language, or both) to provide
/// better review context to the LLM.
///
/// # Configuration
///
/// Controlled via `BundlingConfig` (on `Config`):
/// - `strategy`: `"smart"` (coalesce by directory/language) or `"flat"` (legacy)
/// - `max_chars_per_group`: character budget per group
/// - `max_files_per_group`: file count limit per group
/// - `coalesce_by_directory`: prefer keeping same-directory files together
/// - `coalesce_by_language`: prefer keeping same-language files together
///
/// # Usage
///
/// ```ignore
/// use crate::engine::bundling::{group_files, types::BundlingConfig};
///
/// let config = BundlingConfig::default();
/// let groups = group_files(&files, &config);
/// for group in groups {
///     let content = crate::engine::scanner::format_batch_for_prompt(&group.files);
///     // send content to LLM
/// }
/// ```
// The grouping engine is not called by review yet (#792).
#[allow(dead_code)]
pub mod grouping;
#[allow(dead_code)]
pub mod types;

pub use types::GroupingStrategy;
