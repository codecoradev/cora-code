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
//! The active backend is selected at **runtime** via [`resolve_backend`],
//! which reads the `brain.embedding` config value. At compile time, only
//! the availability of the pretrained path is gated by the `pretrained-embed`
//! feature flag.

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

/// Runtime embedding backend selector.
///
/// Resolved from `brain.embedding` config value at call time.
/// Falls back gracefully when a requested backend is not compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 256-dim hashing trick (always available, zero dependency).
    Hashing,
    /// 768-dim nomic distilled pretrained (requires `pretrained-embed` feature).
    #[allow(dead_code)]
    Pretrained,
}

impl Backend {
    /// Returns the embedding dimensionality for this backend.
    pub const fn dims(self) -> usize {
        match self {
            Self::Hashing => EMBEDDING_DIM,
            #[cfg(feature = "pretrained-embed")]
            Self::Pretrained => PRETRAINED_DIM,
            #[cfg(not(feature = "pretrained-embed"))]
            Self::Pretrained => EMBEDDING_DIM, // unreachable — resolve never returns Pretrained without feature
        }
    }

    /// Returns a human-readable label for this backend.
    pub const fn provider_name(self) -> &'static str {
        match self {
            Self::Hashing => "hashing-trick (256d, static)",
            #[cfg(feature = "pretrained-embed")]
            Self::Pretrained => "nomic-embed-code (768d, pretrained)",
            #[cfg(not(feature = "pretrained-embed"))]
            Self::Pretrained => "hashing-trick (256d, pretrained not compiled)",
        }
    }
}

/// Thread-local active backend — set once at index/brain-search time.
static ACTIVE_BACKEND: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();

/// Resolve the runtime backend from config string.
///
/// Logic:
/// - `"auto"` → pretrained if compiled, else hashing
/// - `"hashing"` → always hashing
/// - `"pretrained"` → pretrained if compiled, else hashing + warning
///
/// The result is cached process-wide via `OnceLock`.
pub fn resolve_backend(config_embedding: &str) -> Backend {
    // Already resolved? Return cached value.
    if let Some(&b) = ACTIVE_BACKEND.get() {
        return b;
    }

    let backend = match config_embedding {
        "hashing" => Backend::Hashing,
        "pretrained" => {
            #[cfg(feature = "pretrained-embed")]
            {
                Backend::Pretrained
            }
            #[cfg(not(feature = "pretrained-embed"))]
            {
                tracing::warn!(
                    "brain.embedding=pretrained but cora was not compiled with --features pretrained-embed; \
                     falling back to hashing-trick 256d"
                );
                Backend::Hashing
            }
        }
        // "auto" or any unknown value
        _ => {
            #[cfg(feature = "pretrained-embed")]
            {
                Backend::Pretrained
            }
            #[cfg(not(feature = "pretrained-embed"))]
            {
                Backend::Hashing
            }
        }
    };

    let _ = ACTIVE_BACKEND.set(backend);
    tracing::debug!(
        config = config_embedding,
        backend = ?backend,
        "resolved embedding backend"
    );
    backend
}

/// Returns the embedding dimensionality used by the active backend.
///
/// Convenience wrapper around `resolve_backend().dims()`.
pub fn active_dims() -> usize {
    // Use a sensible default if resolve_backend hasn't been called yet.
    ACTIVE_BACKEND.get().map(|b| b.dims()).unwrap_or_else(|| {
        #[cfg(feature = "pretrained-embed")]
        {
            PRETRAINED_DIM
        }
        #[cfg(not(feature = "pretrained-embed"))]
        {
            EMBEDDING_DIM
        }
    })
}

/// Returns a human-readable label for the active embedding provider.
pub fn active_provider_name() -> &'static str {
    ACTIVE_BACKEND
        .get()
        .map(|b| b.provider_name())
        .unwrap_or_else(|| {
            #[cfg(feature = "pretrained-embed")]
            {
                "nomic-embed-code (768d, pretrained)"
            }
            #[cfg(not(feature = "pretrained-embed"))]
            {
                "hashing-trick (256d, static)"
            }
        })
}

/// Embed a code snippet using the best available backend.
///
/// Returns an f32 vector that can be passed directly to usearch.
///
/// Dispatches to the backend set by [`resolve_backend`]. If no backend has
/// been explicitly resolved, falls back to compile-time default.
pub fn embed_code_dispatch(code: &str) -> Vec<f32> {
    let backend = ACTIVE_BACKEND.get().copied().unwrap_or_else(|| {
        // Lazy resolve with "auto" if not yet set
        resolve_backend("auto")
    });

    match backend {
        Backend::Hashing => {
            let embedding = tokens::embed_code(code);
            embedding.as_slice().iter().map(|&v| v as f32).collect()
        }
        #[cfg(feature = "pretrained-embed")]
        Backend::Pretrained => embed_code_pretrained(code),
        #[cfg(not(feature = "pretrained-embed"))]
        Backend::Pretrained => {
            // Should never happen — resolve_backend never returns Pretrained without feature
            let embedding = tokens::embed_code(code);
            embedding.as_slice().iter().map(|&v| v as f32).collect()
        }
    }
}

/// Whether the pretrained embedding backend is available (compile-time).
#[allow(dead_code)]
pub const fn has_pretrained() -> bool {
    cfg!(feature = "pretrained-embed")
}
