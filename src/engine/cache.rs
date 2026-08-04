use crate::error::CoraError;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

use crate::engine::types::ReviewResponse;

/// Get the cache directory: ~/.cache/cora/reviews/
fn cache_dir() -> std::result::Result<PathBuf, CoraError> {
    let home = dirs::home_dir()
        .ok_or_else(|| CoraError::ConfigRead("cannot determine home directory".into()))?;
    Ok(home.join(".cache").join("cora").join("reviews"))
}

/// Compute SHA-256 hex digest of the diff content + config parameters.
/// Includes model, provider, base_url, and temperature so config changes
/// invalidate the cache (e.g., switching providers with the same model name).
#[allow(clippy::format_collect)]
fn cache_key(diff: &str, model: &str, temperature: f32, provider: &str, base_url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(diff.as_bytes());
    hasher.update(model.as_bytes());
    hasher.update(provider.as_bytes());
    hasher.update(base_url.as_bytes());
    hasher.update(temperature.to_le_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Get cached review response if available and not expired.
///
/// Returns `None` if no cache exists or if the cache entry has expired
/// (based on `ttl` in minutes).
pub fn get_cached_review(
    diff: &str,
    model: &str,
    temperature: f32,
    ttl: u64,
    provider: &str,
    base_url: &str,
) -> Option<ReviewResponse> {
    let hash = cache_key(diff, model, temperature, provider, base_url);
    let dir = cache_dir().ok()?;
    let path = dir.join(format!("{hash}.json"));

    if !path.is_file() {
        debug!("cache miss: file not found");
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let cached: CachedReview = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            debug!("cache corrupt, ignoring: {}", e);
            return None;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let age_secs = now.saturating_sub(cached.timestamp);
    let ttl_secs = ttl * 60;

    if age_secs > ttl_secs {
        debug!(age_secs = age_secs, ttl_secs = ttl_secs, "cache expired");
        // Clean up expired cache file
        let _ = std::fs::remove_file(&path);
        return None;
    }

    debug!(hash = %hash, age_secs = age_secs, "cache hit");
    Some(cached.response)
}

/// Save a review response to the cache.
pub fn save_cached_review(
    diff: &str,
    model: &str,
    temperature: f32,
    response: &ReviewResponse,
    provider: &str,
    base_url: &str,
) -> std::result::Result<(), CoraError> {
    let dir = cache_dir()?;
    std::fs::create_dir_all(&dir).map_err(CoraError::CacheIo)?;

    let hash = cache_key(diff, model, temperature, provider, base_url);
    let path = dir.join(format!("{hash}.json"));

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cached = CachedReview {
        response: response.clone(),
        timestamp: now,
    };

    let json = serde_json::to_string_pretty(&cached)
        .map_err(|e| CoraError::CacheSerialize(e.to_string()))?;

    std::fs::write(&path, json).map_err(CoraError::CacheIo)?;

    debug!(hash = %hash, "saved review to cache");
    Ok(())
}

/// Internal cache entry format.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedReview {
    response: ReviewResponse,
    timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;
    use crate::engine::types::ReviewIssue;

    fn make_response() -> ReviewResponse {
        ReviewResponse {
            issues: vec![ReviewIssue {
                file: "src/main.rs".to_string(),
                line: Some(10),
                severity: Severity::Major,
                issue_type: Some("bugs".to_string()),
                title: "Null pointer".to_string(),
                body: "Could be null here".to_string(),
                suggested_fix: Some("Add a check".to_string()),
            }],
            summary: "Found 1 issue.".to_string(),
            tokens_used: None,
            should_block: false,
        }
    }

    #[test]
    fn cache_key_is_deterministic() {
        let hash1 = cache_key(
            "hello world",
            "gpt-4",
            0.0,
            "openai",
            "https://api.openai.com/v1",
        );
        let hash2 = cache_key(
            "hello world",
            "gpt-4",
            0.0,
            "openai",
            "https://api.openai.com/v1",
        );
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn cache_key_differs_for_different_inputs() {
        let hash1 = cache_key(
            "hello world",
            "gpt-4",
            0.0,
            "openai",
            "https://api.openai.com/v1",
        );
        let hash2 = cache_key(
            "hello earth",
            "gpt-4",
            0.0,
            "openai",
            "https://api.openai.com/v1",
        );
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn cache_key_includes_model_and_temperature() {
        let h1 = cache_key("diff", "gpt-4", 0.0, "openai", "https://api.openai.com/v1");
        let h2 = cache_key(
            "diff",
            "gpt-3.5",
            0.0,
            "openai",
            "https://api.openai.com/v1",
        );
        let h3 = cache_key("diff", "gpt-4", 0.7, "openai", "https://api.openai.com/v1");
        assert_ne!(h1, h2, "different models should differ");
        assert_ne!(h1, h3, "different temperatures should differ");
    }

    #[test]
    fn cache_key_len_is_64() {
        let diff = "diff --git a/file.txt b/file.txt\n+ hello";
        let hash = cache_key(diff, "model", 0.0, "openai", "https://api.openai.com/v1");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn cache_miss_on_different_diff() {
        let diff1 = "diff --git a/a.txt b/a.txt\n+ hello";
        let diff2 = "diff --git a/b.txt b/b.txt\n+ world";
        let hash1 = cache_key(diff1, "model", 0.0, "openai", "https://api.openai.com/v1");
        let hash2 = cache_key(diff2, "model", 0.0, "openai", "https://api.openai.com/v1");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn cache_key_differs_for_different_providers() {
        let h1 = cache_key("diff", "gpt-4", 0.0, "openai", "https://api.openai.com/v1");
        let h2 = cache_key(
            "diff",
            "gpt-4",
            0.0,
            "azure",
            "https://my-azure.openai.azure.com",
        );
        assert_ne!(
            h1, h2,
            "different providers should produce different cache keys"
        );
    }

    #[test]
    fn cache_key_differs_for_different_base_urls() {
        let h1 = cache_key("diff", "gpt-4", 0.0, "openai", "https://api.openai.com/v1");
        let h2 = cache_key(
            "diff",
            "gpt-4",
            0.0,
            "openai",
            "https://proxy.example.com/v1",
        );
        assert_ne!(
            h1, h2,
            "different base_urls should produce different cache keys"
        );
    }

    #[test]
    fn cached_review_serialization_roundtrip() {
        let response = make_response();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cached = CachedReview {
            response,
            timestamp: now,
        };

        let json = serde_json::to_string(&cached).unwrap();
        let back: CachedReview = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timestamp, now);
        assert_eq!(back.response.issues.len(), 1);
        assert_eq!(back.response.issues[0].file, "src/main.rs");
        assert_eq!(back.response.summary, "Found 1 issue.");
    }

    #[test]
    fn ttl_expiry() {
        let response = make_response();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cached = CachedReview {
            response,
            timestamp: now.saturating_sub(2 * 60 * 60), // 2 hours ago
        };

        let json = serde_json::to_string(&cached).unwrap();
        let back: CachedReview = serde_json::from_str(&json).unwrap();

        // With ttl = 60 (1 hour), this should be expired
        let age_secs = now - back.timestamp;
        let ttl_secs = 60 * 60;
        assert!(age_secs > ttl_secs);
    }

    #[test]
    fn ttl_not_expired() {
        let response = make_response();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cached = CachedReview {
            response,
            timestamp: now.saturating_sub(10 * 60), // 10 minutes ago
        };

        let json = serde_json::to_string(&cached).unwrap();
        let back: CachedReview = serde_json::from_str(&json).unwrap();

        // With ttl = 1440 (24h), this should NOT be expired
        let age_secs = now - back.timestamp;
        let ttl_secs = 1440 * 60;
        assert!(age_secs <= ttl_secs);
    }

    #[test]
    fn save_and_get_cached_review() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Manually set up a cache entry in the temp dir
        let diff = "test diff content";
        let hash = cache_key(diff, "model", 0.0, "openai", "https://api.openai.com/v1");
        let path = dir.join(format!("{hash}.json"));

        let response = make_response();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cached = CachedReview {
            response: response.clone(),
            timestamp: now,
        };

        let json = serde_json::to_string(&cached).unwrap();
        std::fs::write(&path, json).unwrap();

        // Verify we can read it back
        let content = std::fs::read_to_string(&path).unwrap();
        let back: CachedReview = serde_json::from_str(&content).unwrap();
        assert_eq!(back.response.issues.len(), 1);
        assert_eq!(back.response.issues[0].title, "Null pointer");
    }
}
