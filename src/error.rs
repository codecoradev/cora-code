use thiserror::Error;

/// Typed error enum for the cora engine layer.
///
/// Engine functions return `Result<T, CoraError>`. The CLI/command layer
/// continues to use `anyhow::Result` and converts via `?` (automatic
/// `From<CoraError> for anyhow::Error` provided by `thiserror` + anyhow interop).
#[derive(Debug, Error)]
pub enum CoraError {
    /// HTTP request to LLM API failed (network, DNS, timeout, etc.)
    #[error("LLM API request failed: {0}")]
    LlmRequest(#[from] reqwest::Error),

    /// LLM API returned a non-2xx status code.
    #[error("LLM API returned status {status}: {body}")]
    LlmStatus { status: u16, body: String },

    /// Failed to parse LLM JSON response.
    #[error("failed to parse LLM JSON response: {0}")]
    LlmParse(String),

    /// Error during streaming.
    #[error("streaming error: {0}")]
    LlmStream(String),

    /// YAML config parse error.
    #[error("config parse error: {0}")]
    ConfigParse(String),

    /// Config file read error.
    #[error("config read error: {0}")]
    ConfigRead(String),

    /// No API key configured.
    #[error("no API key found")]
    NoApiKey,

    /// Invalid git ref (shell metacharacters or path traversal).
    #[error("invalid ref: {0}")]
    InvalidRef(String),

    /// Git command execution failed.
    #[error("git command `{command}` failed: {stderr}")]
    GitCommand { command: String, stderr: String },

    /// Not inside a git repository.
    #[error("not inside a git repository")]
    NotInGitRepo,

    /// HEAD is detached (cannot determine branch name).
    #[error("HEAD is detached — cannot determine branch name")]
    HeadDetached,

    /// Cache I/O error (read/write filesystem operations).
    #[error("cache I/O error: {0}")]
    CacheIo(#[from] std::io::Error),

    /// JSON serialization of cache failed.
    #[error("cache serialize error: {0}")]
    CacheSerialize(String),

    /// Auth file read/write error.
    #[error("auth error: {0}")]
    AuthError(String),
}
// test comment for cora review
