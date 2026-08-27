use crate::error::CoraError;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use tracing::debug;

use crate::engine::types::{LLMConfig, ReviewIssue, ReviewResponse, TokenUsage};

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

/// OpenAI-compatible chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Request body for /chat/completions (kept for reference; unused after migration to dynamic json!).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<Value>,
}

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
    message: ChatMessage,
}

/// Usage statistics from the LLM API response.
///
/// Constructed via [`parse_usage_value`] which accepts a raw `serde_json::Value`
/// and handles providers that send legacy field names (`prompt_tokens`,
/// `completion_tokens`), new field names (`input_tokens`, `output_tokens`),
/// or both simultaneously (e.g. GPT-5.4).
#[derive(Debug, Clone, Default)]
pub(crate) struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
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
fn parse_usage_value(val: &Value) -> Option<Usage> {
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
fn usage_to_token_usage(u: &Usage) -> crate::engine::types::TokenUsage {
    crate::engine::types::TokenUsage {
        input_tokens: u.effective_input(),
        output_tokens: u.effective_output(),
        estimated_cost_usd: 0.0,
    }
}

/// System prompt for code review.
const REVIEW_SYSTEM_PROMPT: &str = r#"You are an expert code reviewer providing thorough, actionable feedback on code diffs.

CRITICAL CONSTRAINTS:
1. You MUST ONLY comment on files that appear in the diff. Do NOT invent or hallucinate file paths.
2. Each issue MUST have a clear, descriptive title (one brief sentence, max 100 chars).
3. Report any issue where you can point to SPECIFIC CODE in the diff that is wrong or risky.
   Do NOT report speculative concerns without concrete evidence from the diff.
   When in doubt, downgrade severity rather than omitting — a borderline concern is a valid minor/info finding.
4. Common patterns to always check: unvalidated inputs, missing error handling, resource leaks, race conditions, off-by-one errors, unchecked edge cases.

LANGUAGE-SPECIFIC FALSE POSITIVE AWARENESS:
- In Rust, `Vec::retain()`, `Vec::append()`, `Vec::retain_mut()`, `Vec::splice()`, `Vec::dedup()`, `Vec::sort()`, `Vec::sort_by()` mutate the vector IN-PLACE. Do NOT flag code as "missing assignment" or "result ignored" when these methods are called — the mutation is the intended side effect.
- In Rust, `Err` arms that return early (e.g. `Err(e) => return error_response(...)`) are ERROR HANDLING paths. Do NOT flag them for missing post-conditions (like "filter not applied") — no data flows through error paths.
- In general, distinguish happy paths from error/early-return paths. Post-conditions (filters, transformations, validations) only need to hold on the happy path, not on every match arm.

SEVERITY LEVELS:
- "critical": Security vulnerabilities, crashes, data loss, breaking bugs
- "major": Bugs that affect functionality, logic errors, missing error handling, significant problems
- "minor": Style issues, small nitpicks, minor improvements, borderline concerns backed by evidence
- "info": Suggestions, optional enhancements

FOCUS AREAS (in priority order):
1. Security vulnerabilities (SQL injection, XSS, auth issues, data exposure, unsafe deserialization)
2. Bugs and logic errors (off-by-one, null handling, race conditions, incorrect conditions, missing edge cases)
3. Error handling (unchecked results, swallowed errors, missing cleanup on failure paths)
4. Performance problems (inefficient algorithms, memory leaks, N+1 queries, unnecessary allocations)
5. Best practices (idiomatic code, naming, DRY, separation of concerns)

RESPONSE FORMAT:
Return a JSON array of objects with these fields:
- "file": string — the file path (MUST be from the diff)
- "line": number or null — the approximate line number
- "severity": "critical" | "major" | "minor" | "info"
- "issue_type": string — category (security, performance, bugs, best_practice, style, suggestion)
- "title": string — short description (max 100 chars)
- "body": string — detailed explanation with specific code reference
- "suggested_fix": string or null — optional fix suggestion

EXPLANATION STYLE (moderate-explanation principle, arXiv:2607.24601):
Keep each finding at moderate depth: severity + a short reason (1-3
sentences) + the specific code evidence it points to. Do NOT include
long reasoning chains, step-by-step derivations, or exhaustive
justifications — overly long explanations reduce agreement with the
finding without adding value. Trust the reader to reason from the
evidence.

If no issues are found, return: []

Return ONLY the JSON array. No markdown code fences, no explanation, no conversational text.
Start with [ and end with ]."#;

/// System prompt for full project scanning.
const SCAN_SYSTEM_PROMPT: &str = r#"You are an expert code reviewer performing a full project scan. Analyze the provided code files and identify issues.

CRITICAL CONSTRAINTS:
1. You MUST ONLY comment on files that were provided to you. Do NOT invent file paths.
2. Each issue MUST have a clear, descriptive title (one brief sentence, max 100 chars).
3. If uncertain whether something is a real issue, omit it rather than guessing.

SEVERITY LEVELS:
- "critical": Security vulnerabilities, crashes, data loss, breaking bugs
- "major": Bugs that affect functionality, significant problems
- "minor": Style issues, small nitpicks, minor improvements
- "info": Suggestions, optional enhancements

FOCUS AREAS (in priority order):
1. Security vulnerabilities (SQL injection, XSS, auth issues, data exposure)
2. Bugs and logic errors (off-by-one, null handling, race conditions)
3. Performance problems (inefficient algorithms, memory leaks, N+1 queries)
4. Best practices (idiomatic code, error handling, naming)

RESPONSE FORMAT:
Return a JSON array of objects with these fields:
- "file": string — the file path (MUST be from the provided files)
- "line": number or null — the approximate line number
- "severity": "critical" | "major" | "minor" | "info"
- "issue_type": string — category (security, performance, bugs, best_practice, style, suggestion)
- "title": string — short description (max 100 chars)
- "body": string — detailed explanation
- "suggested_fix": string or null — optional fix suggestion

Also include a "summary" string at the end after a "|||" separator:
[...JSON array...]|||Summary text here.

If no issues are found, return: []|||No issues found.

Return ONLY this format. No markdown code fences, no conversational text.
Start the JSON array with [ and end with ]."#;

/// Send a chat completion request to an OpenAI-compatible API.
///
/// Returns `(content, usage)` where `usage` is the token statistics reported
/// by the provider (or `None` if the provider omits the `usage` field).
async fn chat_completion(
    config: &LLMConfig,
    system_prompt: &str,
    user_message: &str,
    spinner: Option<&ProgressBar>,
    response_format: &str,
) -> std::result::Result<(String, Option<Usage>), CoraError> {
    let client = shared_client();

    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

    if let Some(sp) = spinner {
        sp.set_message(format!(
            "Sending to {} ({})…",
            config.provider, config.model
        ));
    }

    let mut request = serde_json::json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_message }
        ],
        "temperature": config.temperature,
    });
    request[config.max_tokens_param.clone()] = serde_json::json!(config.max_tokens);

    if response_format == "json_object" {
        request["response_format"] = serde_json::json!({"type": "json_object"});
    }

    debug!(model = %config.model, url = %url, "sending LLM request");

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&request)
        .timeout(std::time::Duration::from_secs(config.timeout))
        .send()
        .await
        .map_err(CoraError::LlmRequest)?;

    let status = response.status();
    let body = response.text().await.map_err(CoraError::LlmRequest)?;

    if !status.is_success() {
        return Err(CoraError::LlmStatus {
            status: status.as_u16(),
            body,
        });
    }

    if let Some(sp) = spinner {
        sp.set_message("Parsing response…");
    }

    let parsed: ChatResponse =
        serde_json::from_str(&body).map_err(|e| CoraError::LlmParse(format!("{e}: {body}")))?;

    let content = parsed
        .choices
        .first()
        .map(|c| c.message.content.clone())
        .unwrap_or_default();

    let usage = parsed.usage.as_ref().and_then(parse_usage_value);

    debug!(tokens = ?usage, "LLM response received");
    tracing::Span::current().record("tokens_used", usage.as_ref().map(|u| u.total_tokens));

    Ok((content, usage))
}

/// Create an animated spinner for LLM operations.
///
/// Automatically hidden when stderr is not a TTY (piped/redirected),
/// preventing ANSI pollution in captured output.
fn create_spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    // Hide spinner when stderr is not a terminal (piped/redirected)
    if !atty_check() {
        spinner.set_draw_target(ProgressDrawTarget::hidden());
        return spinner;
    }
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .expect("valid spinner template")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ "),
    );
    spinner.set_message(message.to_string());
    spinner
}

/// Check if stderr is connected to a TTY.
fn atty_check() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// Raw chat completion — returns the raw string response.
/// Used by commit message generation and other non-review tasks.
///
/// Token usage is intentionally discarded; callers that need it should use
/// [`chat_completion`] directly.
pub async fn chat_completion_raw(
    llm_config: &LLMConfig,
    system_prompt: &str,
    user_message: &str,
) -> std::result::Result<String, CoraError> {
    chat_completion(llm_config, system_prompt, user_message, None, "none")
        .await
        .map(|(content, _)| content)
}

/// Raw streaming chat completion — collects the full stream and returns the response string.
///
/// Token usage is intentionally discarded; callers that need it should use
/// [`chat_completion_stream`] directly.
pub async fn chat_completion_stream_raw(
    llm_config: &LLMConfig,
    system_prompt: &str,
    user_message: &str,
) -> std::result::Result<String, CoraError> {
    chat_completion_stream(llm_config, system_prompt, user_message, "none")
        .await
        .map(|(content, _)| content)
}

/// Review a diff using the LLM. Returns a `ReviewResponse`.
#[allow(clippy::too_many_arguments)]
pub async fn review_diff(
    llm_config: &LLMConfig,
    diff: &str,
    focus: &[String],
    rules: &[String],
    response_format: &str,
    system_prompt_override: Option<&str>,
    quiet: bool,
    static_context: Option<&str>,
) -> std::result::Result<ReviewResponse, CoraError> {
    let spinner = if quiet {
        None
    } else {
        Some(create_spinner("Reviewing diff…"))
    };

    let enclosing = enclosing_section(diff);
    let user_prompt = build_review_prompt(diff, focus, rules, static_context, Some(&enclosing));

    let system_prompt = system_prompt_override.unwrap_or(REVIEW_SYSTEM_PROMPT);

    let (raw, usage) = chat_completion(
        llm_config,
        system_prompt,
        &user_prompt,
        spinner.as_ref(),
        response_format,
    )
    .await?;

    let parse_result = parse_review_response(&raw, usage.as_ref());
    match parse_result {
        Ok(result) => {
            if let Some(sp) = spinner {
                sp.finish_and_clear();
            }
            Ok(ReviewResponse {
                issues: result.0,
                summary: result.1,
                tokens_used: result.2,
                should_block: false,
            })
        }
        Err(e) => {
            // LLM produced invalid JSON — retry once with stricter prompt
            debug!(error = %e, "first parse attempt failed, retrying LLM request");
            if let Some(sp) = &spinner {
                sp.set_message("Retrying (parse error)…");
            }
            let strict_prompt = format!(
                "{}\n\nIMPORTANT: Your response MUST contain only valid JSON. \
                Ensure all strings use proper JSON escape sequences. \
                Do NOT use raw backslashes in string values.",
                user_prompt
            );
            let (retry_raw, retry_usage) = chat_completion(
                llm_config,
                system_prompt,
                &strict_prompt,
                spinner.as_ref(),
                response_format,
            )
            .await?;
            let (issues, summary, tokens_used) =
                parse_review_response(&retry_raw, retry_usage.as_ref())?;
            if let Some(sp) = spinner {
                sp.finish_and_clear();
            }
            Ok(ReviewResponse {
                issues,
                summary,
                tokens_used,
                should_block: false,
            })
        }
    }
}

/// Review a diff using the LLM with streaming. Returns a `ReviewResponse`.
///
/// Streams tokens from the LLM and prints them to stdout in real-time,
/// then collects the full response for parsing.
#[allow(clippy::too_many_arguments)]
pub async fn review_diff_stream(
    llm_config: &LLMConfig,
    diff: &str,
    focus: &[String],
    rules: &[String],
    response_format: &str,
    system_prompt_override: Option<&str>,
    static_context: Option<&str>,
) -> std::result::Result<ReviewResponse, CoraError> {
    let enclosing = enclosing_section(diff);
    let user_prompt = build_review_prompt(diff, focus, rules, static_context, Some(&enclosing));

    let system_prompt = system_prompt_override.unwrap_or(REVIEW_SYSTEM_PROMPT);

    let (raw, usage) =
        chat_completion_stream(llm_config, system_prompt, &user_prompt, response_format).await?;

    let (issues, summary, tokens_used) = parse_review_response(&raw, usage.as_ref())?;

    println!(); // trailing newline after streamed output

    Ok(ReviewResponse {
        issues,
        summary,
        tokens_used,
        should_block: false,
    })
}

/// Send a streaming chat completion request to an OpenAI-compatible API.
///
/// Sends `"stream": true` in the request body, reads SSE chunks, prints
/// delta content to stdout in real-time, and returns the full accumulated text.
#[allow(clippy::too_many_lines)]
async fn chat_completion_stream(
    config: &LLMConfig,
    system_prompt: &str,
    user_message: &str,
    response_format: &str,
) -> std::result::Result<(String, Option<Usage>), CoraError> {
    use futures_util::StreamExt;
    use std::io::Write;

    let client = shared_client();
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

    let mut request_body = serde_json::json!({
        "model": config.model,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_message }
        ],
        "temperature": config.temperature,
        "stream": true,
        // Ask OpenAI-compatible providers to include token usage in the final
        // SSE chunk. Providers that don't recognise this field simply ignore it.
        "stream_options": { "include_usage": true }
    });
    request_body[config.max_tokens_param.clone()] = serde_json::json!(config.max_tokens);

    if response_format == "json_object" {
        request_body["response_format"] = serde_json::json!({"type": "json_object"});
    }

    debug!(model = %config.model, url = %url, "sending streaming LLM request");

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .timeout(std::time::Duration::from_secs(config.timeout))
        .send()
        .await
        .map_err(CoraError::LlmRequest)?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(CoraError::LlmStatus {
            status: status.as_u16(),
            body,
        });
    }

    let mut stream = response.bytes_stream();

    // Buffer for assembling lines from byte chunks
    let mut line_buf = String::new();
    let mut accumulated = String::new();
    // Token usage reported in the final chunk (if the provider supports it).
    let mut final_usage: Option<Usage> = None;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| CoraError::LlmStream(e.to_string()))?;
        let chunk_str = String::from_utf8_lossy(&chunk);

        // Process the chunk character by character to handle line boundaries
        for ch in chunk_str.chars() {
            if ch == '\n' {
                let line = line_buf.trim().to_string();
                line_buf.clear();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    if data.trim() == "[DONE]" {
                        debug!(
                            accumulated_len = accumulated.len(),
                            has_usage = final_usage.is_some(),
                            "streaming complete"
                        );
                        return Ok((accumulated, final_usage));
                    }

                    match serde_json::from_str::<Value>(data) {
                        Ok(parsed) => {
                            if let Some(c) = extract_stream_content(&parsed) {
                                if !c.is_empty() {
                                    // Print delta chunk immediately for live streaming effect
                                    print!("{c}");
                                    let _ = std::io::stdout().flush();
                                    accumulated.push_str(c);
                                }
                            }
                            if let Some(u) = extract_stream_usage(&parsed) {
                                final_usage = Some(u);
                            }
                        }
                        Err(e) => {
                            debug!("skipping unparseable SSE chunk: {e}");
                        }
                    }
                }
            } else {
                line_buf.push(ch);
            }
        }
    }

    // Process any remaining partial line
    if !line_buf.trim().is_empty() {
        let line = line_buf.trim();
        if let Some(data) = line.strip_prefix("data: ") {
            if data.trim() != "[DONE]" {
                if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                    if let Some(c) = extract_stream_content(&parsed) {
                        if !c.is_empty() {
                            print!("{c}");
                            let _ = std::io::stdout().flush();
                            accumulated.push_str(c);
                        }
                    }
                    if let Some(u) = extract_stream_usage(&parsed) {
                        final_usage = Some(u);
                    }
                }
            }
        }
    }

    debug!(
        accumulated_len = accumulated.len(),
        has_usage = final_usage.is_some(),
        "streaming complete"
    );
    Ok((accumulated, final_usage))
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

/// Scan a batch of file contents using the LLM. Returns issues found.
#[allow(clippy::format_push_string)]
pub async fn scan_files(
    llm_config: &LLMConfig,
    files_content: &str,
    focus: &[String],
    rules: &[String],
    response_format: &str,
    system_prompt_override: Option<&str>,
    brain_context: Option<&str>,
) -> std::result::Result<(Vec<ReviewIssue>, Option<String>, Option<TokenUsage>), CoraError> {
    let spinner = create_spinner("Scanning files…");

    let system_prompt = system_prompt_override.unwrap_or(SCAN_SYSTEM_PROMPT);

    let mut user_prompt = String::new();
    if !focus.is_empty() {
        user_prompt.push_str(&format!("Focus areas: {}\n\n", focus.join(", ")));
    }
    if !rules.is_empty() {
        user_prompt.push_str(&format!(
            "Additional rules:\n{}\n\n",
            rules
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    // Inject brain/code-intel context when available (impact analysis,
    // related patterns, affected tests from the symbol index).
    if let Some(ctx) = brain_context {
        if !ctx.is_empty() {
            user_prompt.push_str("## Code Intelligence (Brain)\n");
            user_prompt.push_str(ctx);
            user_prompt.push_str("\n\n");
        }
    }
    user_prompt.push_str("Files to review:\n\n");
    user_prompt.push_str(files_content);

    let (raw, usage) = chat_completion(
        llm_config,
        system_prompt,
        &user_prompt,
        Some(&spinner),
        response_format,
    )
    .await?;

    parse_scan_response(&raw, usage.as_ref())
}

/// Extract file paths from a unified diff string.
/// Matches lines like `--- a/path/file.rs` and `+++ b/path/file.rs`.
pub(crate) fn extract_file_paths_from_diff(diff: &str) -> Vec<String> {
    let mut paths = std::collections::HashSet::new();
    for line in diff.lines() {
        let trimmed = line.trim_start();
        // Match unified diff headers: `--- a/path` or `+++ b/path`
        // Also handles `--- path` without a/ or b/ prefix (some diffs)
        let (prefix, strip_ab) = if let Some(rest) = trimmed.strip_prefix("--- a/") {
            (rest, true)
        } else if let Some(rest) = trimmed.strip_prefix("+++ b/") {
            (rest, true)
        } else if let Some(rest) = trimmed.strip_prefix("--- ") {
            (rest, false)
        } else if let Some(rest) = trimmed.strip_prefix("+++ ") {
            (rest, false)
        } else {
            continue;
        };
        // Skip /dev/null (binary files, deletes)
        if prefix.starts_with("/dev/null") {
            continue;
        }
        let path = if strip_ab {
            prefix.to_string()
        } else {
            // Strip a/ or b/ prefix if present
            prefix
                .strip_prefix("a/")
                .or_else(|| prefix.strip_prefix("b/"))
                .unwrap_or(prefix)
                .to_string()
        };
        // Strip trailing \t (git shows tabs for renamed files)
        let path = path.split('\t').next().unwrap_or(&path);
        if !path.is_empty() {
            paths.insert(path.to_string());
        }
    }
    paths.into_iter().collect()
}

/// Build the user prompt for diff review.
#[allow(clippy::format_push_string)]
/// Always-on prompt guardrail (#523): stop plausible-but-wrong reachability
/// claims that come from reasoning over diff hunks alone.
pub(crate) const CONTROL_FLOW_GUARDRAIL: &str = "Control-flow guardrail: do NOT claim an execution path is unreachable or \
that a call is missing on a branch unless the surrounding code confirms it — \
shared match/if arms are reached by every producer feeding them.";

/// Build the enclosing-scope prompt section for a diff (#523).
///
/// Reads post-image files relative to CWD (diff paths are repo-rooted);
/// returns an empty string when no hunk qualifies or files are unreadable.
pub(crate) fn enclosing_section(diff: &str) -> String {
    let snippets =
        crate::engine::enclosing::extract_enclosing_snippets(diff, std::path::Path::new("."));
    if snippets.is_empty() {
        return String::new();
    }
    crate::engine::enclosing::render_for_prompt(&snippets, |f| {
        std::fs::read_to_string(f)
            .map(|c| c.lines().map(String::from).collect())
            .ok()
    })
}

pub(crate) fn build_review_prompt(
    diff: &str,
    focus: &[String],
    rules: &[String],
    static_context: Option<&str>,
    enclosing_context: Option<&str>,
) -> String {
    let mut prompt = String::new();

    // Inject valid file paths to reduce hallucination
    let file_paths = extract_file_paths_from_diff(diff);
    if !file_paths.is_empty() {
        prompt.push_str("Valid files in this diff:\n");
        for path in &file_paths {
            prompt.push_str(&format!("- \"{path}\"\n"));
        }
        prompt.push('\n');
    }

    // Inject static analysis context (clippy output, etc.)
    if let Some(ctx) = static_context {
        if !ctx.is_empty() {
            prompt.push_str("Static analysis context (pre-verified by compiler/linter):\n");
            prompt.push_str("---\n");
            prompt.push_str(ctx);
            prompt.push_str("\n---\n\n");
        }
    }

    // Inject enclosing-scope code for branching hunks (#523)
    if let Some(ctx) = enclosing_context {
        if !ctx.is_empty() {
            prompt.push_str(ctx);
            prompt.push('\n');
        }
    }

    if !focus.is_empty() {
        prompt.push_str(&format!("Focus areas: {}\n\n", focus.join(", ")));
    }

    if !rules.is_empty() {
        prompt.push_str("Additional review rules:\n");
        for rule in rules {
            prompt.push_str(&format!("- {rule}\n"));
        }
        prompt.push('\n');
    }

    prompt.push_str(CONTROL_FLOW_GUARDRAIL);
    prompt.push_str("\n\n");

    prompt.push_str("Review the following diff:\n\n```diff\n");
    prompt.push_str(diff);
    prompt.push_str("\n```\n");

    prompt
}

/// Parse the LLM response into review issues.
/// Handles: raw JSON array, JSON wrapped in markdown fences, array and summary format.
#[allow(clippy::type_complexity)]
pub(crate) fn parse_review_response(
    raw: &str,
    usage: Option<&Usage>,
) -> std::result::Result<(Vec<ReviewIssue>, String, Option<TokenUsage>), CoraError> {
    let (json_str, summary) = extract_json_and_summary(raw);

    // Strip markdown code fences if present
    let json_str = strip_code_fences(&json_str);

    // Repair common LLM JSON mistakes before strict parse
    let json_str = repair_json_string(&json_str);

    // Attempt strict parse first; if it fails due to truncation, try to repair
    let issues: Vec<ReviewIssue> = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("EOF") || err_msg.contains("unexpected end") {
                debug!(error = %err_msg, "attempting truncated JSON repair");
                let repaired = repair_truncated_json(&json_str);
                match serde_json::from_str(&repaired) {
                    Ok(v) => {
                        debug!("truncated JSON repair succeeded — some data may be partial");
                        v
                    }
                    Err(repair_err) => {
                        return Err(CoraError::LlmParse(format!(
                            "parse failed (original: {err_msg}, after repair: {repair_err})"
                        )));
                    }
                }
            } else {
                return Err(CoraError::LlmParse(e.to_string()));
            }
        }
    };

    let tokens_used = usage.map(usage_to_token_usage);
    Ok((issues, summary, tokens_used))
}

/// Parse the LLM response for scan mode.
#[allow(clippy::type_complexity)]
pub(crate) fn parse_scan_response(
    raw: &str,
    usage: Option<&Usage>,
) -> std::result::Result<(Vec<ReviewIssue>, Option<String>, Option<TokenUsage>), CoraError> {
    // Fast-fail when the response is clearly not JSON (e.g. provider error page,
    // empty body, rate-limit message, or prose wrapper). Surfacing the raw
    // prefix lets users diagnose whether it's truncation, a provider error,
    // or HTML.
    if !looks_like_json_array(raw) {
        return Err(CoraError::LlmParse(non_json_error_message(raw)));
    }

    let (json_str, summary) = extract_json_and_summary(raw);
    let json_str = strip_code_fences(&json_str);

    // Repair common LLM JSON mistakes before strict parse
    let json_str = repair_json_string(&json_str);

    // Attempt strict parse first; if it fails, try repair + partial extraction
    let issues: Vec<ReviewIssue> = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            let err_msg = e.to_string();
            let is_truncation = err_msg.contains("EOF")
                || err_msg.contains("unexpected end")
                || err_msg.contains("expected");

            if is_truncation {
                debug!(error = %err_msg, "attempting JSON repair for scan response");

                // Try repair first
                let repaired = repair_truncated_json(&json_str);
                match serde_json::from_str(&repaired) {
                    Ok(v) => {
                        debug!("truncated JSON repair succeeded — some data may be partial");
                        v
                    }
                    Err(repair_err) => {
                        debug!(error = %repair_err, "repair failed, trying partial object extraction");

                        // Last resort: extract individual complete JSON objects
                        // from the truncated response. This recovers valid
                        // findings that appeared before truncation.
                        let partials = extract_partial_json_objects(&json_str);
                        if !partials.is_empty() {
                            let mut recovered = Vec::with_capacity(partials.len());
                            let mut parse_errors = 0;
                            for obj_str in &partials {
                                match serde_json::from_str::<ReviewIssue>(obj_str) {
                                    Ok(issue) => recovered.push(issue),
                                    Err(_) => parse_errors += 1,
                                }
                            }
                            if !recovered.is_empty() {
                                debug!(
                                    recovered = recovered.len(),
                                    skipped_partial = parse_errors,
                                    "partial JSON object extraction recovered findings from truncated response"
                                );
                                recovered
                            } else {
                                return Err(CoraError::LlmParse(format!(
                                    "parse failed (original: {err_msg}, after repair: {repair_err}). Could not recover any valid objects from {} partial objects. Raw response prefix: {}",
                                    partials.len(),
                                    preview_raw(raw)
                                )));
                            }
                        } else {
                            return Err(CoraError::LlmParse(format!(
                                "parse failed (original: {err_msg}, after repair: {repair_err}). No complete JSON objects found in truncated response. Raw response prefix: {}",
                                preview_raw(raw)
                            )));
                        }
                    }
                }
            } else {
                return Err(CoraError::LlmParse(format!(
                    "{err_msg}. Raw response prefix: {}",
                    preview_raw(raw)
                )));
            }
        }
    };

    let summary = if summary.is_empty() {
        None
    } else {
        Some(summary)
    };

    let tokens_used = usage.map(usage_to_token_usage);
    Ok((issues, summary, tokens_used))
}

/// Check whether a raw LLM response plausibly contains a JSON payload.
///
/// Accepts responses that (after trimming leading whitespace and optional
/// markdown fences) begin with `[` or `{`. Rejects obvious non-JSON bodies
/// such as HTML error pages, empty strings, or pure prose.
pub(crate) fn looks_like_json_array(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    // Strip a leading ```json or ``` fence if present
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    matches!(stripped.chars().next(), Some('[') | Some('{'))
}

/// Build a human-readable diagnostic for a non-JSON LLM response, including a
/// truncated preview of the raw body (first 512 bytes) so users can tell
/// whether the provider returned an error page, rate-limit message, or prose.
pub(crate) fn non_json_error_message(raw: &str) -> String {
    let len = raw.len();
    format!(
        "LLM response is not valid JSON (length={len}). This usually means the provider returned an error body, rate-limit page, or truncated output. Raw response prefix: {}",
        preview_raw(raw)
    )
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

/// Extract JSON and optional summary (after ||| separator).
fn extract_json_and_summary(raw: &str) -> (String, String) {
    if let Some(idx) = raw.find("|||") {
        let json_part = raw[..idx].trim().to_string();
        let summary_part = raw[idx + 3..].trim().to_string();
        (json_part, summary_part)
    } else {
        // Try to find the JSON array boundaries
        let trimmed = raw.trim();
        if trimmed.starts_with('[') {
            // Find the matching closing bracket
            let mut depth = 0;
            let mut end = 0;
            for (i, c) in trimmed.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if end > 0 {
                let json_part = trimmed[..end].to_string();
                let summary_part = trimmed[end..].trim().to_string();
                return (json_part, summary_part);
            }
        }
        (trimmed.to_string(), String::new())
    }
}

/// Repair common LLM JSON mistakes before strict parse.
///
/// LLMs sometimes produce JSON with invalid escape sequences (e.g. lone backslashes
/// like `\s` or trailing `\` inside string values). This function applies minimal
/// fixes so `serde_json` can parse the output.
fn repair_json_string(json_str: &str) -> String {
    // Replace lone backslashes inside JSON string values that aren't valid JSON escapes.
    // Valid JSON escapes: \" \\ \/ \b \f \n \r \t \uXXXX
    let repaired = repair_invalid_escapes(json_str);
    if repaired == json_str {
        json_str.to_string()
    } else {
        debug!("applied backslash repair to LLM JSON output");
        repaired
    }
}

/// Repair truncated JSON by closing unclosed strings, arrays, and objects.
///
/// When an LLM response is cut off due to max_tokens, the JSON is often
/// incomplete — unclosed string values, missing `]` or `}` brackets.
/// This function walks the JSON tracking nesting depth and string state,
/// then appends the necessary closing characters.
fn repair_truncated_json(json: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escape_next = false;

    for ch in json.chars() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' | '[' if !in_string => stack.push(ch),
            '}' if !in_string && stack.last() == Some(&'{') => {
                stack.pop();
            }
            ']' if !in_string && stack.last() == Some(&'[') => {
                stack.pop();
            }
            _ => {}
        }
    }

    let mut repaired = json.to_string();

    // Close unclosed string
    if in_string {
        repaired.push('"');
    }

    // Close brackets in reverse order
    for ch in stack.iter().rev() {
        match ch {
            '{' => repaired.push('}'),
            '[' => repaired.push(']'),
            _ => {}
        }
    }

    repaired
}

/// Extract individual complete JSON objects from a potentially truncated JSON array.
///
/// When an LLM response is truncated mid-array (e.g. `[{"file":"a",...}, {"file":"b",`),
/// `repair_truncated_json` may produce syntactically valid but semantically broken JSON
/// (the truncated object has a partial string value). This function takes a different
/// approach: it walks the JSON character-by-character and extracts every *complete*
/// top-level object (balanced braces, respecting strings and escapes). Each extracted
/// object is then parsed individually — partial/invalid tail objects are discarded.
fn extract_partial_json_objects(json: &str) -> Vec<String> {
    let trimmed = json.trim_start();
    let trimmed = trimmed
        .strip_prefix('[')
        .or_else(|| trimmed.strip_prefix("```json\n["))
        .or_else(|| trimmed.strip_prefix("```\n["))
        .unwrap_or(trimmed);

    let mut objects = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;
    let mut obj_start = None;

    for (i, ch) in trimmed.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start.take() {
                        objects.push(trimmed[start..=i].to_string());
                    }
                }
            }
            _ => {}
        }
    }

    objects
}

/// Replace invalid escape sequences in JSON string values.
/// Tracks whether we're inside a string literal using a proper state machine
/// that handles escaped quotes correctly.
fn repair_invalid_escapes(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                output.push(c);
                // Scan through string literal
                loop {
                    match chars.next() {
                        Some('\\') => {
                            // Escape character — check what follows
                            match chars.peek() {
                                Some(&next) if is_valid_json_escape(next) => {
                                    output.push('\\');
                                    output.push(next);
                                    chars.next(); // consume
                                    if next == 'u' {
                                        // Consume exactly 4 hex digits
                                        let mut hex_count = 0;
                                        for _ in 0..4 {
                                            if let Some(&hex) = chars.peek() {
                                                if hex.is_ascii_hexdigit() {
                                                    output.push(hex);
                                                    chars.next();
                                                    hex_count += 1;
                                                }
                                            }
                                        }
                                        if hex_count < 4 {
                                            // Invalid \u escape — not enough hex digits
                                            // Remove the \u we already output and repair
                                            output.truncate(output.len() - 2);
                                            output.push_str("\\\\u");
                                            // Re-peek remaining chars that weren't consumed
                                            for _ in 0..(4 - hex_count) {
                                                if let Some(&c) = chars.peek() {
                                                    output.push(c);
                                                    chars.next();
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(&next) => {
                                    // Invalid escape — double the backslash
                                    debug!(
                                        escape_seq = format!("\\{}", next),
                                        "repairing invalid JSON escape"
                                    );
                                    output.push_str("\\\\");
                                    output.push(next);
                                    chars.next(); // consume
                                }
                                None => {
                                    // Trailing backslash at end of input
                                    output.push_str("\\\\");
                                }
                            }
                        }
                        Some('"') => {
                            output.push('"');
                            break; // end of string
                        }
                        Some(ch) => {
                            output.push(ch);
                        }
                        None => {
                            break; // EOF inside string — let serde_json report it
                        }
                    }
                }
            }
            _ => {
                output.push(c);
            }
        }
    }

    output
}

/// Check if a character is a valid JSON escape sequence starter.
fn is_valid_json_escape(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
}

/// Strip ```json / ``` code fences from the response.
fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(stripped) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
    {
        stripped
            .strip_suffix("```")
            .unwrap_or(stripped)
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::Severity;

    const SINGLE_ISSUE_JSON: &str = r#"[{"file":"src/main.rs","line":42,"severity":"critical","issue_type":"security","title":"SQL Injection","body":"User input is concatenated directly into SQL query.","suggested_fix":"Use parameterized queries."}]"#;

    const TWO_ISSUES_JSON: &str = r#"[
  {"file":"src/api.rs","line":10,"severity":"major","issue_type":"performance","title":"N+1 Query","body":"Query inside a loop.","suggested_fix":"Use eager loading."},
  {"file":"src/lib.rs","line":5,"severity":"minor","issue_type":"bugs","title":"Off-by-one","body":"Loop bound is off by one."}
]"#;

    const EMPTY_ARRAY: &str = "[]";

    // ─── extract_json_and_summary ───

    #[test]
    fn extract_json_no_separator() {
        let (json, summary) = extract_json_and_summary(SINGLE_ISSUE_JSON);
        assert!(json.starts_with('['));
        assert!(summary.is_empty());
    }

    #[test]
    fn extract_json_with_separator() {
        let input = format!("{SINGLE_ISSUE_JSON}|||Found 1 critical issue.");
        let (json, summary) = extract_json_and_summary(&input);
        assert!(json.starts_with('['));
        assert_eq!(summary, "Found 1 critical issue.");
    }

    #[test]
    fn extract_json_with_separator_and_whitespace() {
        let input = format!("  {SINGLE_ISSUE_JSON}  |||   Some summary text   ");
        let (json, summary) = extract_json_and_summary(&input);
        assert!(json.starts_with('['));
        assert_eq!(summary, "Some summary text");
    }

    #[test]
    fn extract_json_finds_array_boundaries() {
        // Text after the array but before the |||
        let input = format!("{SINGLE_ISSUE_JSON}\nHere is some trailing text.");
        let (json, summary) = extract_json_and_summary(&input);
        assert!(json.starts_with('[') && json.ends_with(']'));
        assert_eq!(summary, "Here is some trailing text.");
    }

    #[test]
    fn extract_json_empty_separator() {
        let (json, summary) = extract_json_and_summary("[]|||");
        assert_eq!(json, "[]");
        assert_eq!(summary, "");
    }

    // ─── strip_code_fences ───

    #[test]
    fn strip_fences_json() {
        let fenced = "```json\n[{\"a\":1}]\n```";
        assert_eq!(strip_code_fences(fenced), "[{\"a\":1}]");
    }

    #[test]
    fn strip_fences_plain() {
        let fenced = "```\n[{\"a\":1}]\n```";
        assert_eq!(strip_code_fences(fenced), "[{\"a\":1}]");
    }

    #[test]
    fn strip_fences_none() {
        assert_eq!(strip_code_fences("[{\"a\":1}]"), "[{\"a\":1}]");
    }

    #[test]
    fn strip_fences_unclosed() {
        let fenced = "```json\n[{\"a\":1}]";
        assert_eq!(strip_code_fences(fenced), "[{\"a\":1}]");
    }

    // ─── parse_review_response ───

    #[test]
    fn parse_review_clean_json() {
        let result = parse_review_response(SINGLE_ISSUE_JSON, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].file, "src/main.rs");
        assert_eq!(result.0[0].line, Some(42));
        assert_eq!(result.0[0].severity, Severity::Critical);
        assert_eq!(result.1, ""); // no summary
    }

    #[test]
    fn parse_review_with_fences() {
        let input = format!("```json\n{SINGLE_ISSUE_JSON}\n```");
        let result = parse_review_response(&input, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].severity, Severity::Critical);
    }

    #[test]
    fn parse_review_with_pipe_summary() {
        let input = format!("{SINGLE_ISSUE_JSON}|||1 critical security vulnerability found.");
        let result = parse_review_response(&input, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.1, "1 critical security vulnerability found.");
    }

    #[test]
    fn parse_review_empty_array() {
        let result = parse_review_response(EMPTY_ARRAY, None).unwrap();
        assert!(result.0.is_empty());
    }

    #[test]
    fn parse_review_two_issues() {
        let result = parse_review_response(TWO_ISSUES_JSON, None).unwrap();
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[0].severity, Severity::Major);
        assert_eq!(result.0[1].severity, Severity::Minor);
    }

    #[test]
    fn parse_review_malformed_json_errors() {
        let result = parse_review_response("not json at all", None);
        assert!(result.is_err());
    }

    #[test]
    fn parse_review_object_not_array_errors() {
        let result = parse_review_response(r#"{"file":"x"}"#, None);
        assert!(result.is_err());
    }

    #[test]
    fn parse_review_json_with_trailing_text() {
        // The parser should handle trailing text after the array
        let input = format!("{SINGLE_ISSUE_JSON}\nSome extra text");
        let result = parse_review_response(&input, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].file, "src/main.rs");
    }

    // ─── parse_scan_response ───

    #[test]
    fn parse_scan_clean_json() {
        let result = parse_scan_response(SINGLE_ISSUE_JSON, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert!(result.1.is_none()); // no summary → None
    }

    #[test]
    fn parse_scan_with_pipe_summary() {
        let input = format!("{EMPTY_ARRAY}|||No issues found.");
        let result = parse_scan_response(&input, None).unwrap();
        assert!(result.0.is_empty());
        assert_eq!(result.1.as_deref(), Some("No issues found."));
    }

    #[test]
    fn parse_scan_empty_no_summary() {
        let result = parse_scan_response(EMPTY_ARRAY, None).unwrap();
        assert!(result.0.is_empty());
        assert!(result.1.is_none());
    }

    #[test]
    fn parse_scan_with_fences() {
        let input = format!("```json\n{SINGLE_ISSUE_JSON}\n```");
        let result = parse_scan_response(&input, None).unwrap();
        assert_eq!(result.0.len(), 1);
    }

    #[test]
    fn parse_scan_malformed_json_errors() {
        assert!(parse_scan_response("{{invalid", None).is_err());
    }

    // ─── Various severity values ───

    #[test]
    fn parse_all_severities() {
        let input = r#"[
            {"file":"a.rs","line":1,"severity":"critical","issue_type":"security","title":"T1","body":"B1"},
            {"file":"b.rs","line":2,"severity":"major","issue_type":"performance","title":"T2","body":"B2"},
            {"file":"c.rs","line":3,"severity":"minor","issue_type":"bugs","title":"T3","body":"B3"},
            {"file":"d.rs","line":4,"severity":"info","issue_type":"style","title":"T4","body":"B4"}
        ]"#;
        let result = parse_review_response(input, None).unwrap();
        assert_eq!(result.0.len(), 4);
        assert_eq!(result.0[0].severity, Severity::Critical);
        assert_eq!(result.0[1].severity, Severity::Major);
        assert_eq!(result.0[2].severity, Severity::Minor);
        assert_eq!(result.0[3].severity, Severity::Info);
    }

    // ─── Token usage threading (BUG-1) ───

    #[test]
    fn parse_review_preserves_usage_when_provided() {
        // Given a valid JSON response AND usage stats from the API,
        // parse_review_response MUST surface them as Some(TokenUsage).
        // Regression test: previously hardcoded to None.
        let usage = Usage {
            prompt_tokens: 150,
            completion_tokens: 42,
            total_tokens: 192,
        };
        let result = parse_review_response(SINGLE_ISSUE_JSON, Some(&usage)).unwrap();
        let tokens = result
            .2
            .expect("tokens_used should be Some when usage is provided");
        assert_eq!(tokens.input_tokens, 150);
        assert_eq!(tokens.output_tokens, 42);
    }

    #[test]
    fn parse_review_returns_none_usage_when_not_provided() {
        // When the provider doesn't send usage (e.g. some local models),
        // tokens_used must be None, not panic.
        let result = parse_review_response(SINGLE_ISSUE_JSON, None).unwrap();
        assert!(result.2.is_none());
    }

    #[test]
    fn parse_scan_preserves_usage_when_provided() {
        let usage = Usage {
            prompt_tokens: 500,
            completion_tokens: 100,
            total_tokens: 600,
        };
        let result = parse_scan_response(SINGLE_ISSUE_JSON, Some(&usage)).unwrap();
        let tokens = result
            .2
            .expect("tokens_used should be Some when usage is provided");
        assert_eq!(tokens.input_tokens, 500);
        assert_eq!(tokens.output_tokens, 100);
    }

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

    // ─── Various issue_type values ───

    #[test]
    fn parse_various_issue_types() {
        let input = r#"[
            {"file":"a.rs","line":1,"severity":"critical","issue_type":"security","title":"T","body":"B"},
            {"file":"b.rs","line":2,"severity":"major","issue_type":"performance","title":"T","body":"B"},
            {"file":"c.rs","line":3,"severity":"minor","issue_type":"bugs","title":"T","body":"B"},
            {"file":"d.rs","line":4,"severity":"info","issue_type":"best_practice","title":"T","body":"B"},
            {"file":"e.rs","line":5,"severity":"info","issue_type":"style","title":"T","body":"B"}
        ]"#;
        let result = parse_review_response(input, None).unwrap();
        assert_eq!(result.0.len(), 5);
        assert_eq!(result.0[0].issue_type.as_deref(), Some("security"));
        assert_eq!(result.0[1].issue_type.as_deref(), Some("performance"));
        assert_eq!(result.0[2].issue_type.as_deref(), Some("bugs"));
        assert_eq!(result.0[3].issue_type.as_deref(), Some("best_practice"));
        assert_eq!(result.0[4].issue_type.as_deref(), Some("style"));
    }

    // ─── null/optional fields ───

    #[test]
    fn parse_issue_with_null_line() {
        let input = r#"[{"file":"a.rs","line":null,"severity":"info","title":"T","body":"B"}]"#;
        let result = parse_review_response(input, None).unwrap();
        assert_eq!(result.0[0].line, None);
    }

    #[test]
    fn parse_issue_with_null_suggested_fix() {
        let input = r#"[{"file":"a.rs","line":1,"severity":"info","title":"T","body":"B","suggested_fix":null}]"#;
        let result = parse_review_response(input, None).unwrap();
        assert!(result.0[0].suggested_fix.is_none());
    }

    #[test]
    fn parse_issue_with_type_alias() {
        // "type" should also work via serde alias
        let input = r#"[{"file":"a.rs","line":1,"severity":"info","type":"security","title":"T","body":"B"}]"#;
        let result = parse_review_response(input, None).unwrap();
        assert_eq!(result.0[0].issue_type.as_deref(), Some("security"));
    }

    // ─── build_review_prompt ───

    #[test]
    fn build_prompt_basic() {
        let prompt = build_review_prompt("diff content", &[], &[], None, None);
        assert!(prompt.contains("diff content"));
        assert!(prompt.contains("```diff"));
    }

    #[test]
    fn build_prompt_with_focus() {
        let prompt = build_review_prompt("d", &["security".to_string()], &[], None, None);
        assert!(prompt.contains("Focus areas: security"));
    }

    #[test]
    fn build_prompt_with_rules() {
        let prompt = build_review_prompt("d", &[], &["no unwrap".to_string()], None, None);
        assert!(prompt.contains("no unwrap"));
    }

    #[test]
    fn build_prompt_contains_file_paths() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n- old\n+ new";
        let prompt = build_review_prompt(diff, &[], &[], None, None);
        assert!(prompt.contains("Valid files in this diff:"));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn build_prompt_no_file_paths_for_empty_diff() {
        let prompt = build_review_prompt("no diff headers here", &[], &[], None, None);
        assert!(!prompt.contains("Valid files in this diff:"));
    }

    // ─── extract_file_paths_from_diff ───

    #[test]
    fn extract_paths_single_file() {
        let diff = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n- old\n+ new";
        let paths = extract_file_paths_from_diff(diff);
        assert_eq!(paths, vec!["src/main.rs"]);
    }

    #[test]
    fn extract_paths_multiple_files() {
        let diff = "--- a/src/a.rs\n+++ b/src/a.rs\n--- a/src/b.rs\n+++ b/src/b.rs";
        let paths = extract_file_paths_from_diff(diff);
        assert!(paths.contains(&"src/a.rs".to_string()));
        assert!(paths.contains(&"src/b.rs".to_string()));
    }

    #[test]
    fn extract_paths_skips_dev_null() {
        let diff = "--- /dev/null\n+++ b/src/new.rs\n--- a/src/old.rs\n+++ /dev/null";
        let paths = extract_file_paths_from_diff(diff);
        assert!(paths.contains(&"src/new.rs".to_string()));
        assert!(paths.contains(&"src/old.rs".to_string()));
    }

    #[test]
    fn extract_paths_deduplicates() {
        let diff = "--- a/src/main.rs\n+++ b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs";
        let paths = extract_file_paths_from_diff(diff);
        assert_eq!(paths.len(), 1);
    }

    // ─── repair_json_string ───

    #[test]
    fn repair_valid_json_unchanged() {
        let input = r#"[{"file":"a.rs","body":"use std::io;\nlet x = 1;"}]"#;
        assert_eq!(repair_json_string(input), input);
    }

    #[test]
    fn repair_invalid_backslash_in_string() {
        // LLM produced `\s` inside a JSON string — should become `\\s`
        let input = r#"[{"file":"a.rs","body":"regex: \s+"}]"#;
        let repaired = repair_json_string(input);
        // After repair, serde_json should parse it
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["body"], "regex: \\s+");
    }

    #[test]
    fn repair_trailing_backslash_before_close() {
        // LLM produced `\` right before end of string value — the `\` followed by
        // the closing `"` looks like escaped quote, but the actual LLM mistake is
        // different. Test a realistic case: `\s` inside body text.
        // This tests the most common LLM error: non-standard escape like \s
        let input = r#"[{"file":"a.rs","body":"regex: \s+\d*","severity":"info","title":"T","issue_type":"style"}]"#;
        let repaired = repair_json_string(input);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["body"], "regex: \\s+\\d*");
    }

    #[test]
    fn repair_preserves_valid_escapes() {
        // Valid escapes should not be double-escaped
        let input = r#"[{"file":"a.rs","body":"line1\nline2\ttab"}]"#;
        let repaired = repair_json_string(input);
        assert_eq!(repaired, input);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["body"], "line1\nline2\ttab");
    }

    #[test]
    fn repair_invalid_unicode_escape() {
        // \u followed by non-hex — should be escaped
        let input = r#"[{"file":"a.rs","body":"\uGGGG"}]"#;
        let repaired = repair_json_string(input);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["body"], "\\uGGGG");
    }

    #[test]
    fn parse_response_with_invalid_escapes() {
        // End-to-end: LLM response with invalid escapes should parse successfully
        let raw = r#"[{"file":"src/main.rs","line":10,"severity":"critical","issue_type":"security","title":"SQL Injection","body":"query ends with \n no wait \\","suggested_fix":"Use params"}]"#;
        let result = parse_review_response(raw, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].title, "SQL Injection");
    }

    // ─── repair_truncated_json ───

    #[test]
    fn repair_truncated_unclosed_string() {
        let input = r#"[{"file":"main.rs","title":"Bug","body":"incomplete"#;
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["file"], "main.rs");
        assert_eq!(parsed[0]["body"], "incomplete");
    }

    #[test]
    fn repair_truncated_unclosed_array_and_object() {
        let input = r#"[{"file":"main.rs","title":"Bug""#;
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["file"], "main.rs");
    }

    #[test]
    fn repair_truncated_multiple_unclosed_brackets() {
        let input = r#"[{"file":"a.rs","issues":[{"title":"x""#;
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["file"], "a.rs");
    }

    #[test]
    fn repair_truncated_string_with_escaped_quote() {
        let input = r#"[{"file":"main.rs","body":"has \"quote inside"#;
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed[0]["file"], "main.rs");
    }

    #[test]
    fn repair_truncated_nothing_to_fix() {
        let input = r#"[{"file":"main.rs"}]"#;
        let repaired = repair_truncated_json(input);
        assert_eq!(repaired, input);
    }

    #[test]
    fn repair_truncated_after_complete_first_item() {
        // First item complete, second item truncated
        let input = r#"[{"file":"a.rs","line":1,"severity":"critical","issue_type":"security","title":"SQL","body":"bad","suggested_fix":"fix"},{"file":"b.rs","title":"X","body":"incomplete"#;
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["file"], "a.rs");
        assert_eq!(parsed[1]["file"], "b.rs");
    }

    #[test]
    fn repair_truncated_empty_array_unclosed() {
        let input = "[";
        let repaired = repair_truncated_json(input);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&repaired).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn repair_truncated_nested_object_with_string_value() {
        let input = r#"{"findings":[{"file":"a.rs","severity":"critical"}],"summary":"partial"#;
        let repaired = repair_truncated_json(input);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["findings"][0]["file"], "a.rs");
    }

    #[test]
    fn parse_response_truncated_e2e() {
        // End-to-end: truncated LLM response should be repaired and parsed
        let raw = r#"[{"file":"src/main.rs","line":42,"severity":"critical","issue_type":"security","title":"Hardcoded secret","body":"API key found in source","suggested_fix":"Use env vars"},{"file":"src/lib.rs","line":10,"severity":"major","issue_type":"bugs","title":"Unwrap panic","body":"incomplete"#;
        let result = parse_review_response(raw, None).unwrap();
        assert_eq!(result.0.len(), 2);
        assert_eq!(result.0[0].file, "src/main.rs");
        assert_eq!(result.0[0].severity, crate::engine::Severity::Critical);
        assert_eq!(result.0[1].file, "src/lib.rs");
    }

    #[test]
    fn parse_scan_response_truncated_e2e() {
        let raw = r#"[{"file":"config.rs","line":5,"severity":"info","issue_type":"style","title":"Formatting","body":"Bad style"#;
        let result = parse_scan_response(raw, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].file, "config.rs");
    }

    // ─── looks_like_json_array / non-JSON guard (#316) ───

    #[test]
    fn looks_like_json_array_accepts_plain_array() {
        assert!(looks_like_json_array(EMPTY_ARRAY));
        assert!(looks_like_json_array(SINGLE_ISSUE_JSON));
    }

    #[test]
    fn looks_like_json_array_accepts_fenced_json() {
        let fenced = format!("```json\n{SINGLE_ISSUE_JSON}\n```");
        assert!(looks_like_json_array(&fenced));
        let plain_fence = format!("```\n{EMPTY_ARRAY}\n```");
        assert!(looks_like_json_array(&plain_fence));
    }

    #[test]
    fn looks_like_json_array_accepts_leading_whitespace() {
        let padded = format!("\n   \t  {SINGLE_ISSUE_JSON}");
        assert!(looks_like_json_array(&padded));
    }

    #[test]
    fn looks_like_json_array_rejects_empty() {
        assert!(!looks_like_json_array(""));
        assert!(!looks_like_json_array("   \n\t\n"));
    }

    #[test]
    fn looks_like_json_array_rejects_html_error_page() {
        let html = "<html><body><h1>503 Service Unavailable</h1></body></html>";
        assert!(!looks_like_json_array(html));
    }

    #[test]
    fn looks_like_json_array_rejects_prose() {
        let prose = "Sure, here are the issues I found in your code: first, ...";
        assert!(!looks_like_json_array(prose));
    }

    #[test]
    fn parse_scan_response_rejects_non_json_with_preview() {
        let html = "<html><body>Rate limited</body></html>";
        let err = parse_scan_response(html, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not valid JSON"), "msg = {msg}");
        assert!(msg.contains("Rate limited"), "msg = {msg}");
        assert!(msg.contains("length="), "msg = {msg}");
    }

    #[test]
    fn parse_scan_response_rejects_empty_body() {
        let err = parse_scan_response("", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not valid JSON"), "msg = {msg}");
        assert!(msg.contains("length=0"), "msg = {msg}");
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
    fn preview_raw_preserves_short_input() {
        let short = "hello world";
        assert_eq!(preview_raw(short), short);
    }

    #[test]
    fn preview_raw_collapses_whitespace() {
        let messy = "hello\n\t  world\n\n";
        assert_eq!(preview_raw(messy), "hello world");
    }

    // ─── max_tokens_param JSON key naming ───

    #[test]
    fn chat_request_uses_max_output_tokens() {
        // Verify that when max_tokens_param is "max_output_tokens", the JSON
        // body contains "max_output_tokens" (not "max_tokens") as the key.
        let mut body = serde_json::json!({
            "model": "gemini-pro",
            "messages": [
                { "role": "system", "content": "test" },
                { "role": "user", "content": "hello" }
            ],
            "temperature": 0.0,
        });
        let param_name = "max_output_tokens";
        body[param_name] = serde_json::json!(4096);

        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            serialized.contains(r#""max_output_tokens":4096"#),
            "Expected max_output_tokens key in JSON, got: {serialized}"
        );
        assert!(
            !serialized.contains(r#""max_tokens":"#),
            "Should NOT contain hardcoded max_tokens key, got: {serialized}"
        );
    }

    #[test]
    fn chat_request_uses_max_tokens_default() {
        let mut body = serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [
                { "role": "system", "content": "test" },
                { "role": "user", "content": "hello" }
            ],
            "temperature": 0.0,
        });
        body["max_tokens"] = serde_json::json!(8192);

        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            serialized.contains(r#""max_tokens":8192"#),
            "Expected max_tokens key in JSON, got: {serialized}"
        );
    }

    // ─── extract_partial_json_objects ───

    #[test]
    fn extract_partial_complete_array() {
        let json = r#"[
  {"file":"a.rs","line":1,"severity":"major","issue_type":"bugs","title":"A","body":"b"},
  {"file":"b.rs","line":2,"severity":"minor","issue_type":"bugs","title":"B","body":"b"}
]"#;
        let objs = extract_partial_json_objects(json);
        assert_eq!(objs.len(), 2);
        // Each should be valid
        assert!(serde_json::from_str::<serde_json::Value>(&objs[0]).is_ok());
        assert!(serde_json::from_str::<serde_json::Value>(&objs[1]).is_ok());
    }

    #[test]
    fn extract_partial_truncated_second_object() {
        // Second object truncated mid-string — should only extract the first
        let json = r#"[
  {"file":"a.rs","line":1,"severity":"major","issue_type":"bugs","title":"A","body":"valid body"},
  {"file":"b.rs","line":2,"severity":"minor","issue_type":"bugs","title":"B","body":"truncated without closing quote or brace"#;
        let objs = extract_partial_json_objects(json);
        assert_eq!(objs.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&objs[0]).unwrap();
        assert_eq!(parsed["file"], "a.rs");
    }

    #[test]
    fn extract_partial_truncated_mid_string() {
        // Truncation inside a string value with escaped quotes
        let json = r#"[
  {"file":"a.rs","line":1,"severity":"major","issue_type":"bugs","title":"A","body":"has \"escaped\" quotes"},
  {"file":"b.rs","line":2,"severity":"minor","issue_type":"bugs","title":"B","body":"trunc"#;
        let objs = extract_partial_json_objects(json);
        assert_eq!(
            objs.len(),
            1,
            "Should extract only the complete first object"
        );
    }

    #[test]
    fn extract_partial_nested_braces_in_strings() {
        // Braces inside string values should not affect depth tracking
        let json = r#"[
  {"file":"a.rs","line":1,"severity":"info","issue_type":"style","title":"A","body":"function() { /* code */ }"},
  {"file":"b.rs","line":2,"severity":"info","issue_type":"style","title":"B","body":"also { valid }"}
]"#;
        let objs = extract_partial_json_objects(json);
        assert_eq!(objs.len(), 2);
    }

    #[test]
    fn extract_partial_empty_array() {
        assert_eq!(extract_partial_json_objects("[]").len(), 0);
    }

    #[test]
    fn extract_partial_no_complete_objects() {
        // Single object truncated immediately
        let json = r#"[{"file":"truncated"#;
        assert_eq!(extract_partial_json_objects(json).len(), 0);
    }

    #[test]
    fn parse_scan_response_recovers_from_truncation() {
        // Simulates the scenario from issue #383:
        // Truncation inside a nested brace makes repair produce invalid JSON.
        // After repair fails, partial object extraction recovers the first finding.
        let truncated = concat!(
            r#"[{"file":"fixtures.ts","line":90,"severity":"major","#,
            r#""issue_type":"bugs","title":"X","body":"valid body","#,
            r#""suggested_fix":"fix it"},"#,
            r#"{"file":"settings.ts","line":15,"severity":"minor","#,
            r#""issue_type":"bugs","title":"Y","body":{"detail":"trunc"#,
        );

        let result = parse_scan_response(truncated, None);
        assert!(
            result.is_ok(),
            "Should recover partial findings, got: {:?}",
            result.err()
        );
        let (issues, _summary, _tokens) = result.unwrap();
        assert_eq!(issues.len(), 1, "Should recover exactly 1 complete finding");
        assert_eq!(issues[0].file, "fixtures.ts");
        assert_eq!(issues[0].line, Some(90));
    }
}
