//! Code embeddings: the `potion-code-16M-v2` static code model (`potion` +
//! `model_store`), downloaded once into the data directory (pinned and
//! checksummed) and then used offline.
//!
//! There is no fallback embedding. When the model is not available (offline
//! and not yet downloaded, `CORA_EMBED_OFFLINE=1`, or `brain.embedding: off`)
//! the vector signal is off and search answers from keyword and graph signals
//! alone, with a message that says so (#751). A weak substitute measured
//! neutral to harmful when fused with keyword search, so it is not offered.

pub mod model_store;
pub mod potion;

use std::sync::atomic::{AtomicBool, Ordering};

/// Dimensions of the code model's vectors.
pub const EMBEDDING_DIM: usize = 256;

/// The embedding backend of this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The `potion-code-16M-v2` static code model.
    Potion,
    /// No model: the vector signal is off.
    Off,
}

impl Backend {
    /// Dimensions of this backend's vectors (the model's own once loaded).
    pub fn dims(self) -> usize {
        match self {
            Self::Potion => potion::loaded().map_or(EMBEDDING_DIM, potion::PotionModel::dim),
            Self::Off => 0,
        }
    }

    /// Human-readable label.
    pub fn provider_name(self) -> &'static str {
        match self {
            Self::Potion => "potion-code-16M-v2 (256d, static code model)",
            Self::Off => "none (vector signal off)",
        }
    }

    /// Short stable tag: it names the project's vector file, so vectors of
    /// different models never mix, even when their dimensions are equal.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Potion => "potion-code-v2",
            Self::Off => "off",
        }
    }

    /// Tier label stored with the project.
    pub fn tier(self) -> &'static str {
        match self {
            Self::Potion => "potion",
            Self::Off => "off",
        }
    }
}

/// Process-wide backend, set once at index/brain-search time.
static ACTIVE_BACKEND: std::sync::OnceLock<Backend> = std::sync::OnceLock::new();

/// When set, the model is never downloaded in this process (agent hooks must
/// not hang on the network).
static NO_DOWNLOAD: AtomicBool = AtomicBool::new(false);

/// Forbid downloading the model in this process; an already downloaded model
/// is still used.
pub fn forbid_download() {
    NO_DOWNLOAD.store(true, Ordering::Relaxed);
}

/// Try to make the model available and load it.
fn try_potion(allow_download: bool) -> Option<Backend> {
    let allow_download = allow_download && !NO_DOWNLOAD.load(Ordering::Relaxed);
    match model_store::ensure(allow_download).and_then(|dir| potion::install(&dir).map(|_| ())) {
        Ok(()) => Some(Backend::Potion),
        Err(e) => {
            // Offline by choice: no warning for what the user asked for.
            if allow_download && !model_store::offline() {
                eprintln!(
                    "warning: the code embedding model is not available ({e:#}); \
                     the vector signal is off until it is (search uses keywords and the call \
                     graph). CORA_EMBED_OFFLINE=1 silences this."
                );
            }
            tracing::debug!("code embedding model unavailable: {e:#}");
            None
        }
    }
}

/// Resolve the backend from the `brain.embedding` value.
///
/// - `"off"` → no vector signal
/// - anything else (`"auto"`, and the removed `"potion"`, `"hashing"`,
///   `"pretrained"`) → the code model, downloaded on first use; off when it
///   cannot be had
///
/// Index and search entry points call this once (through the index session);
/// it may download the model. The result is cached process-wide.
pub fn resolve_backend(config_embedding: &str) -> Backend {
    resolve(config_embedding, true)
}

fn resolve(config_embedding: &str, allow_download: bool) -> Backend {
    if let Some(&b) = ACTIVE_BACKEND.get() {
        return b;
    }
    let backend = if config_embedding == "off" {
        Backend::Off
    } else {
        try_potion(allow_download).unwrap_or(Backend::Off)
    };
    // `set` fails when another thread resolved first: use whatever won.
    let _ = ACTIVE_BACKEND.set(backend);
    let backend = ACTIVE_BACKEND.get().copied().unwrap_or(backend);
    tracing::debug!(config = config_embedding, backend = ?backend, "resolved embedding backend");
    backend
}

/// The active backend; resolves `auto` without downloading when none is set.
pub fn active_backend() -> Backend {
    ACTIVE_BACKEND
        .get()
        .copied()
        .unwrap_or_else(|| resolve("auto", false))
}

/// Dimensions of the active backend's vectors.
pub fn active_dims() -> usize {
    active_backend().dims()
}

/// Label of the active embedding provider.
pub fn active_provider_name() -> &'static str {
    active_backend().provider_name()
}

/// Embed one snippet. Empty when the vector signal is off.
pub fn embed_code_dispatch(code: &str) -> Vec<f32> {
    match (active_backend(), potion::loaded()) {
        (Backend::Potion, Some(m)) => m.embed(code),
        _ => Vec::new(),
    }
}

/// Embed many snippets at once (the model batches its lookups). Empty vectors
/// when the vector signal is off.
pub fn embed_batch_dispatch(texts: &[String]) -> Vec<Vec<f32>> {
    match (active_backend(), potion::loaded()) {
        (Backend::Potion, Some(m)) => m.embed_batch(texts),
        _ => texts.iter().map(|_| Vec::new()).collect(),
    }
}
