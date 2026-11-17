//! Deprecated `bundling:` configuration (no effect, kept so existing configs load).
//!
//! The file grouping engine that used it was removed in #792: review splits a large
//! diff by a character budget (`review.max_chunk_chars`), not by directory or language.

pub mod types;

pub use types::GroupingStrategy;
