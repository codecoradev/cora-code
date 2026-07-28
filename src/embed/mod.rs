//! Bag-of-tokens embedding engine.
//!
//! Provides two backends:
//!
//! 1. **Hashing trick** (`tokens` module) — zero-dependency, 256-dim, fast but
//!    low-quality.  Suitable for dedup / near-duplicate detection.
//!
//! 2. **Pre-trained nomic-embed-code** (`token_vocab` module) — real 768-dim
//!    embeddings distilled from [nomic-ai/nomic-embed-code](https://huggingface.co/nomic-ai/nomic-embed-code),
//!    compiled into the binary via `include_bytes!` / `include_str!`.
//!    Higher quality at the cost of ~30 MB binary size.
//!
//! Re-exports will be added to this module in Phase 3 when `cora brain`
//! commands are wired up.

// Embedding engine — consumed by brain module (Phase 3).

pub mod tokens;

// Pre-trained nomic-embed-code module — only compiled when vendored data
// exists. Excluded from crates.io package (too large for 10 MB upload limit).
#[cfg(feature = "pretrained-embed")]
pub mod token_vocab;
