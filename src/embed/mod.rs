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
//! The [`embed_code_dispatch`] function selects the best available backend at
//! compile time: pretrained-embed (768d) → hashing trick (256d) → FTS5-only.

pub mod tokens;

// Pre-trained nomic-embed-code module — only compiled when vendored data
// exists. Excluded from crates.io package (too large for 10 MB upload limit).
#[cfg(feature = "pretrained-embed")]
pub mod token_vocab;

// Re-export tokenizer — used by both backends.
pub use tokens::EMBEDDING_DIM;

// Re-export pretrained constants when feature is enabled.
#[cfg(feature = "pretrained-embed")]
pub use token_vocab::{PRETRAINED_DIM, embed_code_pretrained};

/// Returns the embedding dimensionality used by the active backend.
///
/// - `pretrained-embed` feature → 768
/// - default (hashing trick) → 256
pub const fn active_dims() -> usize {
    #[cfg(feature = "pretrained-embed")]
    {
        PRETRAINED_DIM
    }
    #[cfg(not(feature = "pretrained-embed"))]
    {
        EMBEDDING_DIM
    }
}

/// Returns a human-readable label for the active embedding provider.
pub const fn active_provider_name() -> &'static str {
    #[cfg(feature = "pretrained-embed")]
    {
        "nomic-embed-code (768d, pretrained)"
    }
    #[cfg(not(feature = "pretrained-embed"))]
    {
        "hashing-trick (256d, static)"
    }
}

/// Embed a code snippet using the best available backend.
///
/// Returns an f32 vector that can be passed directly to usearch.
///
/// - **Pretrained path** (`pretrained-embed` feature): tokenises → looks up
///   each token in the nomic vocabulary → accumulates int8 vectors → L2-normalises.
///   Returns 768-dim vector.
///
/// - **Hashing-trick fallback**: tokenises → hashes each token into a
///   pseudo-random 256-dim vector → accumulates → L2-normalises.
///   Returns 256-dim vector.
///
/// Both paths share the same [`tokenize_code`] tokenizer.
pub fn embed_code_dispatch(code: &str) -> Vec<f32> {
    #[cfg(feature = "pretrained-embed")]
    {
        embed_code_pretrained(code)
    }
    #[cfg(not(feature = "pretrained-embed"))]
    {
        let embedding = tokens::embed_code(code);
        embedding.as_slice().iter().map(|&v| v as f32).collect()
    }
}

/// Whether the pretrained embedding backend is available (compile-time).
#[expect(
    dead_code,
    reason = "used by Phase 3+ features; embed module not yet wired at call sites"
)]
pub const fn has_pretrained() -> bool {
    cfg!(feature = "pretrained-embed")
}
