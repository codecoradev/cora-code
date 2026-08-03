//! Pre-trained token vocabulary from nomic-embed-code.
//!
//! Provides real 768-dimensional code embeddings distilled from
//! [nomic-ai/nomic-embed-code](https://huggingface.co/nomic-ai/nomic-embed-code).
//!
//! ## Binary format
//!
//! `code_vectors.bin` starts with an 8-byte little-endian header:
//! ```text
//! [u32 LE: token_count] [u32 LE: dimensionality]
//! ```
//! followed by `token_count × dimensionality` int8 values representing
//! unit-normalised vectors scaled by 127.
//!
//! `code_tokens.txt` contains one token string per line (exactly
//! `token_count` lines), in 1:1 correspondence with the vectors.
//!
//! ## Lookup strategy
//!
//! Tokens are loaded into a `HashMap<String, usize>` mapping token text →
//! vector index.  At embed time, each code token is lowercased and looked up;
//! matched tokens contribute their pre-trained int8 vector (cast to f32 and
//! divided by 127.0) weighted by frequency.  The result is L2-normalised.

use std::collections::HashMap;

// ─── Constants ───────────────────────────────────────────────────────────────

/// Number of tokens in the pre-trained vocabulary.
pub const VOCAB_SIZE: usize = 40_856;

/// Dimensionality of each pre-trained vector.
pub const PRETRAINED_DIM: usize = 768;

// ─── Vendored data (compiled into the binary) ────────────────────────────────

/// Raw binary blob: 8-byte header + VOCAB_SIZE × PRETRAINED_DIM int8 values.
static VECTORS_BIN: &[u8] = include_bytes!("../../vendored/nomic/code_vectors.bin");

/// Token vocabulary: one token per line, exactly VOCAB_SIZE lines.
static TOKENS_TXT: &str = include_str!("../../vendored/nomic/code_tokens.txt");

// ─── Lazy statics ────────────────────────────────────────────────────────────

/// Token → vector-index lookup table. Built once on first access.
static TOKEN_MAP: std::sync::OnceLock<HashMap<String, usize>> = std::sync::OnceLock::new();

/// Returns the token → index lookup, initialising on first call.
///
/// The vocabulary contains one duplicate (empty string appears 11 times);
/// the first occurrence wins.  The map therefore has `VOCAB_SIZE - 10`
/// entries (40 846 unique keys).  This is fine — duplicate vectors are
/// identical so any index gives the same result.
fn token_map() -> &'static HashMap<String, usize> {
    TOKEN_MAP.get_or_init(|| {
        let mut map = HashMap::with_capacity(VOCAB_SIZE);
        for (i, line) in TOKENS_TXT.lines().enumerate() {
            if i >= VOCAB_SIZE {
                break;
            }
            // First occurrence wins for duplicate tokens.
            map.entry(line.to_lowercase()).or_insert(i);
        }
        map
    })
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// A 768-dimensional embedding using pre-trained nomic-embed-code vectors.
#[derive(Debug, Clone)]
pub struct PretrainedEmbedding {
    vec: Vec<f32>,
}

impl Default for PretrainedEmbedding {
    fn default() -> Self {
        Self {
            vec: vec![0.0f32; PRETRAINED_DIM],
        }
    }
}

impl PretrainedEmbedding {
    /// Returns a reference to the underlying f32 vector.
    pub fn as_slice(&self) -> &[f32] {
        &self.vec
    }

    /// Consumes self and returns the underlying vector.
    pub fn into_vec(self) -> Vec<f32> {
        self.vec
    }
}

/// Look up the pre-trained int8 vector for a token index (zero-copy into the
/// static binary blob).
///
/// # Panics
/// Panics if `idx >= VOCAB_SIZE`.
#[inline]
fn pretrained_vec_at(idx: usize) -> &'static [i8] {
    assert!(
        idx < VOCAB_SIZE,
        "token index {idx} out of range (vocab={VOCAB_SIZE})"
    );
    let offset = 8 + idx * PRETRAINED_DIM;
    // SAFETY: the static blob is exactly 8 + VOCAB_SIZE * PRETRAINED_DIM bytes
    // and we just verified idx is in bounds.
    let ptr = VECTORS_BIN[offset..].as_ptr() as *const i8;
    unsafe { std::slice::from_raw_parts(ptr, PRETRAINED_DIM) }
}

/// Embed a code snippet using the pre-trained nomic-embed-code vocabulary.
///
/// 1. Tokenises the code with the same tokenizer as [`crate::embed::tokenize_code`].
/// 2. Looks up each token (lowercased) in the pre-trained vocabulary.
/// 3. Accumulates matched vectors (int8 → f32 / 127.0) weighted by frequency.
/// 4. L2-normalises the result.
///
/// Tokens not found in the vocabulary are silently skipped.
pub fn embed_pretrained(tokens: &HashMap<String, u32>) -> PretrainedEmbedding {
    let map = token_map();
    let mut vec = vec![0.0f32; PRETRAINED_DIM];

    for (token, count) in tokens {
        if let Some(&idx) = map.get(token) {
            let weight = *count as f32;
            let int8_vec = pretrained_vec_at(idx);
            for (v, &iv) in vec.iter_mut().zip(int8_vec.iter()) {
                // int8 → f32, undo ×127 scaling
                *v += weight * (iv as f32 / 127.0);
            }
        }
    }

    // L2 normalise
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }

    PretrainedEmbedding { vec }
}

/// Convenience function: tokenize → embed using pretrained vocabulary in one step.
///
/// Returns an f32 vector (768 dimensions, L2-normalised) ready for usearch.
/// This is the pretrained counterpart of [`crate::embed::tokens::embed_code`].
pub fn embed_code_pretrained(code: &str) -> Vec<f32> {
    let tokens = crate::embed::tokens::tokenize_code(code);
    let emb = embed_pretrained(&tokens);
    emb.into_vec()
}

/// Compute cosine similarity between two [`PretrainedEmbedding`]s.
///
/// Because vectors are pre-normalised, this is just the dot product.
/// Returns a value in `[-1, 1]`.
pub fn pretrained_cosine_similarity(a: &PretrainedEmbedding, b: &PretrainedEmbedding) -> f32 {
    a.vec.iter().zip(b.vec.iter()).map(|(x, y)| x * y).sum()
}

// ─── Verification ────────────────────────────────────────────────────────────

/// Verify the binary blob header at runtime.
///
/// Returns `Ok(())` if the header matches expected constants, or an error
/// describing the mismatch.  Intended to be called in tests or at startup.
pub fn verify_binary_format() -> Result<(), String> {
    if VECTORS_BIN.len() < 8 {
        return Err(format!(
            "binary blob too short: {} bytes (need at least 8 for header)",
            VECTORS_BIN.len()
        ));
    }

    let count = u32::from_le_bytes(VECTORS_BIN[0..4].try_into().unwrap()) as usize;
    let dim = u32::from_le_bytes(VECTORS_BIN[4..8].try_into().unwrap()) as usize;
    let expected_size = 8 + count * dim;

    if count != VOCAB_SIZE {
        return Err(format!(
            "token count mismatch: header says {count}, expected {VOCAB_SIZE}"
        ));
    }
    if dim != PRETRAINED_DIM {
        return Err(format!(
            "dimension mismatch: header says {dim}, expected {PRETRAINED_DIM}"
        ));
    }
    if VECTORS_BIN.len() != expected_size {
        return Err(format!(
            "blob size mismatch: {} bytes, expected {expected_size} ({count} tokens × {dim} dims + 8 header)",
            VECTORS_BIN.len()
        ));
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::tokens::tokenize_code;

    #[test]
    fn binary_format_is_valid() {
        verify_binary_format().unwrap();
    }

    #[test]
    fn token_map_size_matches_vocab() {
        // 40856 lines minus 10 duplicate empty-string entries = 40846 unique keys
        assert_eq!(token_map().len(), VOCAB_SIZE - 10);
    }

    #[test]
    fn known_tokens_in_vocab() {
        let map = token_map();
        // These are first/last tokens in the file
        assert!(map.contains_key("aa"));
        assert!(map.contains_key("zzo"));
        // Common code tokens
        assert!(map.contains_key("fn"));
        assert!(map.contains_key("let"));
        assert!(map.contains_key("function"));
        assert!(map.contains_key("return"));
        assert!(map.contains_key("import"));
        assert!(map.contains_key("class"));
    }

    #[test]
    fn embed_pretrained_dimension() {
        let tokens = tokenize_code("fn main() {}");
        let emb = embed_pretrained(&tokens);
        assert_eq!(emb.as_slice().len(), PRETRAINED_DIM);
    }

    #[test]
    fn embed_pretrained_is_normalised() {
        let tokens = tokenize_code("fn add(a: i32, b: i32) -> i32 { a + b }");
        let emb = embed_pretrained(&tokens);
        let norm: f32 = emb.as_slice().iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "expected unit norm, got {norm}");
    }

    #[test]
    fn identical_code_similarity_is_one() {
        let tokens = tokenize_code("fn main() { println!(\"hello\"); }");
        let a = embed_pretrained(&tokens);
        let b = embed_pretrained(&tokens);
        let sim = pretrained_cosine_similarity(&a, &b);
        assert!(
            (sim - 1.0).abs() < 1e-4,
            "identical code should have similarity ~1.0, got {sim}"
        );
    }

    #[test]
    fn similar_code_high_similarity() {
        let a = embed_pretrained(&tokenize_code("fn add(a: i32, b: i32) -> i32 { a + b }"));
        let b = embed_pretrained(&tokenize_code("fn add(x: i64, y: i64) -> i64 { x + y }"));
        let sim = pretrained_cosine_similarity(&a, &b);
        assert!(
            sim > 0.8,
            "similar functions should have high similarity, got {sim}"
        );
    }

    #[test]
    fn dissimilar_code_lower_similarity() {
        let a = embed_pretrained(&tokenize_code("fn add(a: i32, b: i32) -> i32 { a + b }"));
        let b = embed_pretrained(&tokenize_code(
            "SELECT * FROM users WHERE email = 'test@example.com'",
        ));
        let sim = pretrained_cosine_similarity(&a, &b);
        // With real embeddings, Rust vs SQL should be notably different
        assert!(
            sim < 0.95,
            "dissimilar snippets should not be nearly identical, got {sim}"
        );
    }

    #[test]
    fn empty_tokens_embeds_to_zero() {
        let tokens = HashMap::new();
        let emb = embed_pretrained(&tokens);
        let norm: f32 = emb.as_slice().iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(norm < 1e-10, "empty input should produce zero vector");
    }

    #[test]
    fn cosine_similarity_range() {
        let a = embed_pretrained(&tokenize_code("fn foo() {}"));
        let b = embed_pretrained(&tokenize_code("class Bar { constructor() {} }"));
        let sim = pretrained_cosine_similarity(&a, &b);
        assert!(
            (-1.0..=1.0).contains(&sim),
            "cosine similarity must be in [-1, 1], got {sim}"
        );
    }

    #[test]
    fn deterministic_embedding() {
        let tokens = tokenize_code("hello world");
        let a = embed_pretrained(&tokens);
        let b = embed_pretrained(&tokens);
        assert_eq!(a.as_slice(), b.as_slice());
    }

    #[test]
    fn vector_values_in_expected_range() {
        // Spot-check that int8→f32 conversion produces reasonable values
        let vec = pretrained_vec_at(0);
        for &v in vec.iter().take(16) {
            let f = v as f32 / 127.0;
            assert!(
                (-1.0..=1.0).contains(&f),
                "int8 {v} → f32 {f} out of [-1, 1]"
            );
        }
    }
}
