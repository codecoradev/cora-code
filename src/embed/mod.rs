//! Bag-of-tokens embedding engine.
//!
//! Provides three backends:
//!
//! 1. **Hashing trick** (`tokens` module) — zero-dependency, 256-dim, fast but
//!    low-quality.  Suitable for dedup / near-duplicate detection.
//!
//! 0. **potion-code-16M-v2** (`potion` + `model_store`) — the default. A 256-dim
//!    static code model, downloaded once into the data directory on first use
//!    (pinned and checksummed) and then used offline.
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

pub mod model_store;
pub mod potion;
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
/// Falls back gracefully when a requested backend is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 256-dim hashing trick (always available, zero dependency).
    Hashing,
    /// 768-dim nomic distilled pretrained (requires `pretrained-embed` feature).
    #[allow(dead_code)]
    Pretrained,
    /// 256-dim `potion-code-16M-v2` static code model (downloaded once).
    Potion,
}

impl Backend {
    /// Returns the embedding dimensionality for this backend.
    pub fn dims(self) -> usize {
        match self {
            Self::Hashing => EMBEDDING_DIM,
            #[cfg(feature = "pretrained-embed")]
            Self::Pretrained => PRETRAINED_DIM,
            #[cfg(not(feature = "pretrained-embed"))]
            Self::Pretrained => EMBEDDING_DIM, // unreachable — resolve never returns Pretrained without feature
            Self::Potion => potion::loaded().map_or(EMBEDDING_DIM, potion::PotionModel::dim),
        }
    }

    /// Returns a human-readable label for this backend.
    pub fn provider_name(self) -> &'static str {
        match self {
            Self::Hashing => "hashing-trick (256d, static)",
            #[cfg(feature = "pretrained-embed")]
            Self::Pretrained => "nomic-embed-code (768d, pretrained)",
            #[cfg(not(feature = "pretrained-embed"))]
            Self::Pretrained => "hashing-trick (256d, pretrained not compiled)",
            Self::Potion => "potion-code-16M-v2 (256d, static code model)",
        }
    }

    /// Short stable tag of the backend. It names the vector index file and is
    /// part of every symbol's embedding fingerprint, so vectors of different
    /// models never mix, even when their dimensions are equal.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Hashing => "hashing",
            Self::Pretrained => "pretrained",
            Self::Potion => "potion-code-v2",
        }
    }

    /// Tier label stored with the project and used to weigh the vector signal.
    pub fn tier(self) -> &'static str {
        match self {
            Self::Potion => "potion",
            Self::Pretrained => "pretrained",
            Self::Hashing => "static",
        }
    }
}

/// Thread-local active backend — set once at index/brain-search time.
static ACTIVE_BACKEND: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();

/// Try to make the potion model available and load it.
fn try_potion(allow_download: bool) -> Option<Backend> {
    match model_store::ensure(allow_download).and_then(|dir| potion::install(&dir).map(|_| ())) {
        Ok(()) => Some(Backend::Potion),
        Err(e) => {
            // Offline by choice: no warning for what the user asked for.
            if allow_download && !model_store::offline() {
                eprintln!(
                    "warning: the code embedding model is not available ({e:#}); \
                     using a weaker fallback until it is. CORA_EMBED_OFFLINE=1 silences this."
                );
            }
            tracing::debug!("potion model unavailable: {e:#}");
            None
        }
    }
}

/// The best backend that does not need the network: the potion model when it
/// is already on disk, else pretrained (if compiled), else hashing.
fn fallback_backend() -> Backend {
    #[cfg(feature = "pretrained-embed")]
    {
        Backend::Pretrained
    }
    #[cfg(not(feature = "pretrained-embed"))]
    {
        Backend::Hashing
    }
}

/// Resolve the runtime backend from config string.
///
/// Logic:
/// - `"auto"` (default) → potion (downloaded on first use), else pretrained if
///   compiled, else hashing
/// - `"potion"` → potion, same fallback with a warning
/// - `"pretrained"` → pretrained if compiled, else hashing + warning
/// - `"hashing"` → always hashing
///
/// Index and search entry points call this once (through the index session);
/// it may download the model. The result is cached process-wide via `OnceLock`.
pub fn resolve_backend(config_embedding: &str) -> Backend {
    resolve(config_embedding, true)
}

fn resolve(config_embedding: &str, allow_download: bool) -> Backend {
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
        // "potion", "auto" or any unknown value
        _ => try_potion(allow_download).unwrap_or_else(fallback_backend),
    };

    // `set` fails when another thread resolved first: use whatever won.
    let _ = ACTIVE_BACKEND.set(backend);
    let backend = ACTIVE_BACKEND.get().copied().unwrap_or(backend);
    tracing::debug!(
        config = config_embedding,
        backend = ?backend,
        "resolved embedding backend"
    );
    backend
}

/// The active backend; resolves `auto` without downloading when none is set.
pub fn active_backend() -> Backend {
    ACTIVE_BACKEND
        .get()
        .copied()
        .unwrap_or_else(|| resolve("auto", false))
}

/// Returns the embedding dimensionality used by the active backend.
pub fn active_dims() -> usize {
    active_backend().dims()
}

/// Returns a human-readable label for the active embedding provider.
pub fn active_provider_name() -> &'static str {
    active_backend().provider_name()
}

/// Embed a code snippet using the best available backend.
///
/// Returns an f32 vector that can be passed directly to usearch.
///
/// Dispatches to the backend set by [`resolve_backend`]. If no backend has
/// been explicitly resolved, resolves `auto` without downloading.
pub fn embed_code_dispatch(code: &str) -> Vec<f32> {
    match active_backend() {
        Backend::Potion => match potion::loaded() {
            Some(m) => m.embed(code),
            None => hashing_embed(code),
        },
        #[cfg(feature = "pretrained-embed")]
        Backend::Pretrained => embed_code_pretrained(code),
        _ => hashing_embed(code),
    }
}

/// Embed many snippets at once (the potion model batches its lookups).
pub fn embed_batch_dispatch(texts: &[String]) -> Vec<Vec<f32>> {
    use rayon::prelude::*;
    match (active_backend(), potion::loaded()) {
        (Backend::Potion, Some(m)) => m.embed_batch(texts),
        _ => texts.par_iter().map(|t| embed_code_dispatch(t)).collect(),
    }
}

fn hashing_embed(code: &str) -> Vec<f32> {
    let embedding = tokens::embed_code(code);
    embedding.as_slice().iter().map(|&v| v as f32).collect()
}

/// Whether the pretrained embedding backend is available (compile-time).
#[allow(dead_code)]
pub const fn has_pretrained() -> bool {
    cfg!(feature = "pretrained-embed")
}
