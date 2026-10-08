//! Transport: the one place that talks HTTP to an OpenAI-compatible API.
//!
//! The rest of the LLM layer depends only on the [`Transport`] trait: "send
//! this turn, give me back what the model said" ([`Completion`]). That seam
//! lets the findings policy (parse / repair / retry) be driven by a fake
//! transport in tests with no network, and makes streaming vs non-streaming
//! a transport detail rather than a different failure policy.

use crate::error::CoraError;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use tracing::debug;

use super::LlmEvents;
use crate::engine::types::LLMConfig;

/// Shared `reqwest::Client` with connection pooling. Reused across all LLM requests.
/// Created lazily on first use to avoid blocking initialization.
/// Per-request timeout is set via .`timeout()` on the `RequestBuilder`.
///
/// Supports `REQUESTS_CA_BUNDLE` env var for custom CA certificates
/// (corporate proxies with self-signed certs).
static SHARED_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let mut builder = reqwest::Client::builder().pool_max_idle_per_host(4);

    // Support custom CA certificates for corporate proxies.
    // REQUESTS_CA_BUNDLE is the de-facto standard used by Python requests,
    // curl, Node.js, and most HTTP tooling.
    if let Ok(ca_path) = std::env::var("REQUESTS_CA_BUNDLE") {
        match std::fs::read(&ca_path) {
            Ok(ca_data) => match reqwest::Certificate::from_pem(&ca_data) {
                Ok(cert) => {
                    builder = builder.add_root_certificate(cert);
                    tracing::debug!("loaded custom CA bundle from REQUESTS_CA_BUNDLE");
                }
                Err(e) => {
                    tracing::warn!("failed to parse CA bundle {}: {}", ca_path, e);
                }
            },
            Err(e) => {
                tracing::warn!("failed to read CA bundle {}: {}", ca_path, e);
            }
        }
    }

    builder.build().unwrap_or_else(|e| {
        tracing::error!("failed to build shared HTTP client: {}", e);
        reqwest::Client::new()
    })
});

/// Return the shared `reqwest::Client` for LLM API requests.
pub fn shared_client() -> reqwest::Client {
    SHARED_CLIENT.clone()
}

/// Maximum size of a single SSE line.
pub(crate) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
/// Maximum total accumulated streamed response.
pub(crate) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// Response from /chat/completions.
///
/// `usage` is parsed as raw `serde_json::Value` to avoid serde's duplicate-field
/// detection when a provider sends both legacy (`prompt_tokens`) and new
/// (`input_tokens`) field names simultaneously (e.g. GPT-5.4). The value is
/// converted to a typed `Usage` via [`parse_usage_value`] in post-processing.
#[derive(Debug, Clone, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatChoice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// Response-side message: `content` may be ABSENT or null when a reasoning
/// model spends the entire output budget on chain-of-thought (#536), and some
/// providers expose the thinking under `reasoning_content` (string or parts).
#[derive(Debug, Clone, Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<Value>,
}

/// Usage statistics from the LLM API response.
///
/// Constructed via [`parse_usage_value`] which accepts a raw `serde_json::Value`
/// and handles providers that send legacy field names (`prompt_tokens`,
/// `completion_tokens`), new field names (`input_tokens`, `output_tokens`),
/// or both simultaneously (e.g. GPT-5.4).
#[derive(Debug, Clone, Default)]
pub(crate) struct Usage {
    pub(super) prompt_tokens: u32,
    pub(super) completion_tokens: u32,
    pub(super) total_tokens: u32,
}

/// Extract a typed [`Usage`] from a raw `serde_json::Value`.
///
/// Handles three naming conventions that OpenAI-compatible providers use:
///
/// | Field          | Legacy (OpenAI)   | New (GPT-5+)       | CamelCase (Azure)  |
/// |----------------|-------------------|--------------------|--------------------|
/// | input          | `prompt_tokens`   | `input_tokens`     | `promptTokens`     |
/// | output         | `completion_tokens` | `output_tokens`  | `completionTokens` |
/// | total          | `total_tokens`    | `total_tokens`     | `totalTokens`      |
///
/// Some providers (notably GPT-5.4) send **both** legacy and new names for the
/// same value. Direct serde deserialization with aliases would hit serde_json's
/// duplicate-field guard (>= 1.0.120), so we extract manually via `Value`
/// and pick the first non-zero value in preference order.
pub(super) fn parse_usage_value(val: &Value) -> Option<Usage> {
    let obj = val.as_object()?;

    let prompt_tokens = obj
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| obj.get("promptTokens").and_then(|v| v.as_u64()))
        .or_else(|| obj.get("input_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0) as u32;

    let completion_tokens = obj
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| obj.get("completionTokens").and_then(|v| v.as_u64()))
        .or_else(|| obj.get("output_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(0) as u32;

    let total_tokens = obj
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| obj.get("totalTokens").and_then(|v| v.as_u64()))
        .unwrap_or(0) as u32;

    Some(Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    })
}

impl Usage {
    /// Accumulate usage across attempts (a retried request still cost tokens).
    pub(super) fn plus(&self, other: &Usage) -> Usage {
        Usage {
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }

    /// Effective input tokens.
    ///
    /// Prefers `prompt_tokens`; if that's zero but `total_tokens` is non-zero,
    /// and `completion_tokens` is also zero (no breakdown at all), reports the
    /// entire total as input to avoid double-counting. Otherwise derives from
    /// `total - completion`.
    fn effective_input(&self) -> u32 {
        if self.prompt_tokens > 0 {
            self.prompt_tokens
        } else if self.completion_tokens > 0 {
            self.total_tokens.saturating_sub(self.completion_tokens)
        } else {
            // No breakdown at all — report total as input, output stays 0.
            self.total_tokens
        }
    }

    /// Effective output tokens.
    ///
    /// Prefers `completion_tokens`; if that's zero but `prompt_tokens` is
    /// non-zero, derives from `total - prompt`. If both are zero (only total
    /// reported), returns 0 to avoid double-counting with `effective_input`.
    fn effective_output(&self) -> u32 {
        if self.completion_tokens > 0 {
            self.completion_tokens
        } else if self.prompt_tokens > 0 {
            self.total_tokens.saturating_sub(self.prompt_tokens)
        } else {
            0
        }
    }
}

/// Convert a raw API `Usage` into cora's `TokenUsage`.
///
/// `input_tokens` / `output_tokens` map 1:1 to `prompt_tokens` / `completion_tokens`.
/// Cost estimation is intentionally left at `0.0` here — pricing is provider-specific
/// and should be enriched downstream (e.g. by a future pricing table).
pub(super) fn usage_to_token_usage(u: &Usage) -> crate::engine::types::TokenUsage {
    crate::engine::types::TokenUsage {
        input_tokens: u.effective_input(),
        output_tokens: u.effective_output(),
        estimated_cost_usd: 0.0,
    }
}

/// Return a single-line, length-capped preview of a raw LLM response for logs
/// and error messages. Collapses whitespace and caps at 512 bytes.
pub(crate) fn preview_raw(raw: &str) -> String {
    const MAX_BYTES: usize = 512;
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= MAX_BYTES {
        collapsed
    } else {
        // Split at a char boundary <= MAX_BYTES to avoid slicing mid-codepoint.
        let mut end = MAX_BYTES;
        while end > 0 && !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… [truncated]", &collapsed[..end])
    }
}

// ─── Transport seam ──────────────────────────────────────────────────────────

/// One chat turn: a system prompt, a user prompt and an output-token budget.
/// The budget is per turn (not read from config) because the empty-response
/// recovery raises it between attempts.
pub(crate) struct Turn<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub max_tokens: u32,
}

/// What the model said for one turn, before any interpretation.
///
/// `content` may be empty (reasoning models can spend the whole budget on
/// chain-of-thought, #536); `finish_reason` and `reasoning` carry what the
/// recovery policy needs to decide what to do about that.
#[derive(Debug, Clone, Default)]
pub(crate) struct Completion {
    pub content: String,
    pub finish_reason: Option<String>,
    pub reasoning: Option<Value>,
    pub usage: Option<Usage>,
}

/// The seam between "talk to a model" and "interpret what it said".
///
/// Implemented by [`HttpTransport`] (non-streaming and streaming) and by fakes
/// in tests. Callers are generic over it (static dispatch, no boxing).
pub(crate) trait Transport {
    async fn complete(&self, turn: &Turn<'_>) -> Result<Completion, CoraError>;
}

/// Build the JSON request body for `/chat/completions`.
pub(crate) fn build_request_body(
    config: &LLMConfig,
    turn: &Turn<'_>,
    response_format: &str,
    stream: bool,
) -> Value {
    let mut body = serde_json::json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": turn.system },
            { "role": "user", "content": turn.user }
        ],
        "temperature": config.temperature,
    });
    if stream {
        body["stream"] = serde_json::json!(true);
        // Ask OpenAI-compatible providers to include token usage in the final
        // SSE chunk. Providers that don't recognise this field simply ignore it.
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }
    body[config.max_tokens_param.clone()] = serde_json::json!(turn.max_tokens);
    if response_format == "json_object" {
        body["response_format"] = serde_json::json!({"type": "json_object"});
    }
    body
}

/// HTTP implementation of [`Transport`] for an OpenAI-compatible endpoint.
///
/// `stream == true` requests SSE and forwards each content delta to
/// `events.delta` (the caller decides whether that reaches a terminal).
pub(crate) struct HttpTransport<'a> {
    pub config: &'a LLMConfig,
    pub response_format: &'a str,
    pub stream: bool,
    pub events: &'a dyn LlmEvents,
}

impl Transport for HttpTransport<'_> {
    async fn complete(&self, turn: &Turn<'_>) -> Result<Completion, CoraError> {
        if self.stream {
            self.complete_stream(turn).await
        } else {
            self.complete_once(turn).await
        }
    }
}

impl HttpTransport<'_> {
    fn url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, CoraError> {
        shared_client()
            .post(self.url())
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .timeout(std::time::Duration::from_secs(self.config.timeout))
            .send()
            .await
            .map_err(CoraError::LlmRequest)
    }

    async fn complete_once(&self, turn: &Turn<'_>) -> Result<Completion, CoraError> {
        let body = build_request_body(self.config, turn, self.response_format, false);
        self.events.status(&format!(
            "Sending to {} ({})…",
            self.config.provider, self.config.model
        ));
        debug!(model = %self.config.model, url = %self.url(), "sending LLM request");

        let response = self.post(&body).await?;
        let status = response.status();
        let text = response.text().await.map_err(CoraError::LlmRequest)?;
        if !status.is_success() {
            return Err(CoraError::LlmStatus {
                status: status.as_u16(),
                body: preview_raw(&text),
            });
        }

        self.events.status("Parsing response…");
        let parsed: ChatResponse =
            serde_json::from_str(&text).map_err(|e| CoraError::LlmParse(format!("{e}: {text}")))?;
        let usage = parsed.usage.as_ref().and_then(parse_usage_value);
        debug!(tokens = ?usage, "LLM response received");
        tracing::Span::current().record("tokens_used", usage.as_ref().map(|u| u.total_tokens));

        Ok(match parsed.choices.into_iter().next() {
            Some(c) => Completion {
                content: c.message.content.unwrap_or_default(),
                finish_reason: c.finish_reason,
                reasoning: c.message.reasoning_content,
                usage,
            },
            None => Completion {
                usage,
                ..Completion::default()
            },
        })
    }

    async fn complete_stream(&self, turn: &Turn<'_>) -> Result<Completion, CoraError> {
        use futures_util::StreamExt;

        let body = build_request_body(self.config, turn, self.response_format, true);
        debug!(model = %self.config.model, url = %self.url(), "sending streaming LLM request");

        let response = self.post(&body).await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(CoraError::LlmStatus {
                status: status.as_u16(),
                body: preview_raw(&text),
            });
        }

        let mut stream = response.bytes_stream();
        let mut acc = SseAccumulator::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| CoraError::LlmStream(e.to_string()))?;
            acc.push(&String::from_utf8_lossy(&chunk), self.events)?;
            if acc.is_done() {
                break;
            }
        }
        acc.finish(self.events)
    }
}

// ─── SSE decoding ────────────────────────────────────────────────────────────

/// Incremental decoder for an OpenAI-style SSE chat stream.
///
/// Pure (no I/O): feed it text chunks, it forwards content deltas to the
/// supplied [`LlmEvents`] and enforces the line/total size caps (#573).
#[derive(Default)]
pub(crate) struct SseAccumulator {
    line_buf: String,
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    done: bool,
}

impl SseAccumulator {
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Feed one chunk of the response body.
    pub(crate) fn push(&mut self, chunk: &str, events: &dyn LlmEvents) -> Result<(), CoraError> {
        for ch in chunk.chars() {
            if self.done {
                return Ok(());
            }
            if ch == '\n' {
                let line = std::mem::take(&mut self.line_buf);
                self.process_line(line.trim(), events)?;
            } else {
                self.line_buf.push(ch);
                if self.line_buf.len() > MAX_SSE_LINE_BYTES {
                    return Err(CoraError::LlmStream(format!(
                        "SSE line exceeded {MAX_SSE_LINE_BYTES} bytes without a newline"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Flush any trailing partial line and produce the completion.
    pub(crate) fn finish(mut self, events: &dyn LlmEvents) -> Result<Completion, CoraError> {
        if !self.done {
            let line = std::mem::take(&mut self.line_buf);
            self.process_line(line.trim(), events)?;
        }
        debug!(
            accumulated_len = self.content.len(),
            has_usage = self.usage.is_some(),
            "streaming complete"
        );
        Ok(Completion {
            content: self.content,
            finish_reason: self.finish_reason,
            reasoning: (!self.reasoning.is_empty()).then_some(Value::String(self.reasoning)),
            usage: self.usage,
        })
    }

    fn process_line(&mut self, line: &str, events: &dyn LlmEvents) -> Result<(), CoraError> {
        if line.is_empty() || line.starts_with(':') {
            return Ok(());
        }
        let Some(data) = line.strip_prefix("data: ") else {
            return Ok(());
        };
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        match serde_json::from_str::<Value>(data) {
            Ok(parsed) => {
                if let Some(c) = extract_stream_content(&parsed) {
                    if !c.is_empty() {
                        events.delta(c);
                        self.content.push_str(c);
                        if self.content.len() > MAX_STREAM_BYTES {
                            return Err(CoraError::LlmStream(format!(
                                "streamed response exceeded {MAX_STREAM_BYTES} bytes"
                            )));
                        }
                    }
                }
                if let Some(r) = extract_stream_reasoning(&parsed) {
                    self.reasoning.push_str(r);
                }
                if let Some(f) = extract_stream_finish_reason(&parsed) {
                    self.finish_reason = Some(f.to_string());
                }
                if let Some(u) = extract_stream_usage(&parsed) {
                    self.usage = Some(u);
                }
            }
            Err(e) => debug!("skipping unparseable SSE chunk: {e}"),
        }
        Ok(())
    }
}

fn extract_stream_reasoning(parsed: &Value) -> Option<&str> {
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|v| v.as_str())
}

fn extract_stream_finish_reason(parsed: &Value) -> Option<&str> {
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
}

/// Extract the content delta from a parsed SSE chunk.
fn extract_stream_content(parsed: &Value) -> Option<&str> {
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
}

/// Extract token usage from a parsed SSE chunk.
///
/// The `usage` field appears either at top level (OpenAI convention, sent in
/// the final chunk when `stream_options.include_usage` is set) or inside the
/// final choice's delta (some Azure / third-party providers).
///
/// Uses [`parse_usage_value`] to avoid serde's duplicate-field guard when a
/// provider sends both legacy and new field names simultaneously.
fn extract_stream_usage(parsed: &Value) -> Option<Usage> {
    parsed.get("usage").and_then(parse_usage_value).or_else(|| {
        parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("usage"))
            .and_then(parse_usage_value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_to_token_usage_maps_fields_correctly() {
        let usage = Usage {
            prompt_tokens: 111,
            completion_tokens: 222,
            total_tokens: 333,
        };
        let token_usage = usage_to_token_usage(&usage);
        assert_eq!(token_usage.input_tokens, 111);
        assert_eq!(token_usage.output_tokens, 222);
        assert_eq!(token_usage.estimated_cost_usd, 0.0);
    }

    #[test]
    fn usage_to_token_usage_handles_total_only_provider() {
        // Some providers only report total_tokens without prompt/completion breakdown.
        // Cora attributes the entire total to input (output stays 0) to avoid
        // double-counting in downstream cost calculations.
        let usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 500,
        };
        let token_usage = usage_to_token_usage(&usage);
        assert_eq!(token_usage.input_tokens, 500);
        assert_eq!(token_usage.output_tokens, 0);
    }

    #[test]
    fn usage_to_token_usage_handles_partial_breakdown() {
        // Provider reports prompt_tokens but not completion_tokens.
        let usage = Usage {
            prompt_tokens: 300,
            completion_tokens: 0,
            total_tokens: 450,
        };
        let token_usage = usage_to_token_usage(&usage);
        assert_eq!(token_usage.input_tokens, 300);
        assert_eq!(token_usage.output_tokens, 150); // total - prompt
    }

    // ─── parse_usage_value (GPT-5.4 dual-field handling) ───

    #[test]
    fn parse_usage_value_legacy_fields() {
        // Traditional OpenAI format: prompt_tokens / completion_tokens
        let val = serde_json::json!({
            "prompt_tokens": 2615,
            "completion_tokens": 581,
            "total_tokens": 3196
        });
        let usage = parse_usage_value(&val).unwrap();
        assert_eq!(usage.prompt_tokens, 2615);
        assert_eq!(usage.completion_tokens, 581);
        assert_eq!(usage.total_tokens, 3196);
    }

    #[test]
    fn parse_usage_value_new_fields_only() {
        // Some providers only send input_tokens / output_tokens
        let val = serde_json::json!({
            "input_tokens": 1000,
            "output_tokens": 200,
            "total_tokens": 1200
        });
        let usage = parse_usage_value(&val).unwrap();
        assert_eq!(usage.prompt_tokens, 1000);
        assert_eq!(usage.completion_tokens, 200);
        assert_eq!(usage.total_tokens, 1200);
    }

    #[test]
    fn parse_usage_value_gpt54_dual_fields() {
        // GPT-5.4 sends BOTH legacy and new field names — this is the
        // scenario that previously caused serde duplicate-field error.
        let val = serde_json::json!({
            "prompt_tokens": 2615,
            "completion_tokens": 581,
            "total_tokens": 3196,
            "prompt_tokens_details": {"cached_tokens": 0},
            "completion_tokens_details": {"reasoning_tokens": 0},
            "input_tokens": 2615,
            "output_tokens": 581,
            "input_tokens_details": null
        });
        let usage = parse_usage_value(&val).unwrap();
        // Must prefer primary (prompt_tokens) over alias (input_tokens)
        assert_eq!(usage.prompt_tokens, 2615);
        assert_eq!(usage.completion_tokens, 581);
        assert_eq!(usage.total_tokens, 3196);
    }

    #[test]
    fn parse_usage_value_camelcase_fields() {
        // Azure / some third-party providers use camelCase
        let val = serde_json::json!({
            "promptTokens": 500,
            "completionTokens": 100,
            "totalTokens": 600
        });
        let usage = parse_usage_value(&val).unwrap();
        assert_eq!(usage.prompt_tokens, 500);
        assert_eq!(usage.completion_tokens, 100);
        assert_eq!(usage.total_tokens, 600);
    }

    #[test]
    fn parse_usage_value_missing_fields_defaults_to_zero() {
        // Partial usage (e.g. streaming final chunk)
        let val = serde_json::json!({
            "prompt_tokens": 100
        });
        let usage = parse_usage_value(&val).unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn parse_usage_value_non_object_returns_none() {
        let val = serde_json::json!("not an object");
        assert!(parse_usage_value(&val).is_none());

        let val = serde_json::json!(42);
        assert!(parse_usage_value(&val).is_none());
    }

    #[test]
    fn preview_raw_is_truncated_to_max_bytes() {
        // 2000-char prose should be collapsed and capped at 512 bytes.
        let long = "word ".repeat(500);
        let preview = preview_raw(&long);
        assert!(preview.ends_with("… [truncated]"));
        // Hard cap (512 + suffix length).
        assert!(preview.len() < 600);
    }

    #[test]
    fn llm_status_body_is_capped() {
        // Error bodies from an arbitrary host must not be echoed unbounded.
        let long = "x".repeat(5000);
        let capped = preview_raw(&long);
        assert!(capped.len() < 600, "len={}", capped.len());
        assert!(capped.ends_with("[truncated]"));
    }

    // ─── request body (max_tokens param naming, stream flags) ───

    fn turn(max_tokens: u32) -> Turn<'static> {
        Turn {
            system: "sys",
            user: "usr",
            max_tokens,
        }
    }

    #[test]
    fn request_body_uses_configured_max_tokens_param() {
        let cfg = LLMConfig {
            max_tokens_param: "max_output_tokens".to_string(),
            ..LLMConfig::default()
        };
        let body = build_request_body(&cfg, &turn(4096), "none", false);
        assert_eq!(body["max_output_tokens"], 4096);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("stream").is_none());
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn request_body_uses_turn_budget_not_config_budget() {
        let cfg = LLMConfig::default(); // max_tokens 4096
        let body = build_request_body(&cfg, &turn(8192), "json_object", false);
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn stream_request_body_asks_for_usage() {
        let body = build_request_body(&LLMConfig::default(), &turn(100), "none", true);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    // ─── SSE accumulator ───

    use super::super::NoEvents;
    use std::sync::Mutex;

    struct Collect(Mutex<Vec<String>>);
    impl LlmEvents for Collect {
        fn delta(&self, chunk: &str) {
            self.0.lock().unwrap().push(chunk.to_string());
        }
    }

    fn data(v: serde_json::Value) -> String {
        format!("data: {v}\n\n")
    }

    #[test]
    fn sse_collects_content_reasoning_finish_and_usage_across_split_chunks() {
        let sink = Collect(Mutex::new(Vec::new()));
        let body = [
            ": keep-alive\n\n".to_string(),
            data(serde_json::json!({"choices":[{"delta":{"reasoning_content":"think "}}]})),
            data(serde_json::json!({"choices":[{"delta":{"content":"[1,"}}]})),
            data(serde_json::json!({"choices":[{"delta":{"content":"2]"},"finish_reason":"stop"}]})),
            data(serde_json::json!({"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}})),
            "data: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"IGNORED\"}}]}\n\n"
                .to_string(),
        ]
        .concat();
        let mut acc = SseAccumulator::default();
        for piece in body.as_bytes().chunks(5) {
            acc.push(&String::from_utf8_lossy(piece), &sink).unwrap();
        }
        assert!(acc.is_done());
        let c = acc.finish(&sink).unwrap();
        assert_eq!(c.content, "[1,2]");
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
        assert_eq!(c.reasoning, Some(Value::String("think ".into())));
        assert_eq!(c.usage.unwrap().total_tokens, 7);
        assert_eq!(*sink.0.lock().unwrap(), ["[1,", "2]"]);
    }

    #[test]
    fn sse_flushes_trailing_line_without_newline() {
        let mut acc = SseAccumulator::default();
        acc.push(
            "data: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}",
            &NoEvents,
        )
        .unwrap();
        assert_eq!(acc.finish(&NoEvents).unwrap().content, "tail");
    }

    #[test]
    fn sse_line_without_newline_is_capped() {
        let mut acc = SseAccumulator::default();
        let err = acc
            .push(&"x".repeat(MAX_SSE_LINE_BYTES + 1), &NoEvents)
            .unwrap_err();
        assert!(matches!(err, CoraError::LlmStream(m) if m.contains("SSE line exceeded")));
    }

    #[test]
    fn sse_total_stream_is_capped() {
        let mut acc = SseAccumulator::default();
        // Each line stays under the per-line cap; the sum must trip the total cap.
        let big = "y".repeat(MAX_SSE_LINE_BYTES / 2);
        let line = data(serde_json::json!({"choices":[{"delta":{"content": big}}]}));
        let mut result = Ok(());
        for _ in 0..(MAX_STREAM_BYTES / (MAX_SSE_LINE_BYTES / 2) + 2) {
            result = acc.push(&line, &NoEvents);
            if result.is_err() {
                break;
            }
        }
        assert!(matches!(result, Err(CoraError::LlmStream(m)) if m.contains("exceeded")));
    }

    #[test]
    fn preview_raw_preserves_short_input() {
        let short = "hello world";
        assert_eq!(preview_raw(short), short);
    }

    #[test]
    fn preview_raw_collapses_whitespace() {
        let messy = "hello\n\t  world\n\n";
        assert_eq!(preview_raw(messy), "hello world");
    }
}
