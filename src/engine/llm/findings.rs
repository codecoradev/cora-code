//! Structured findings from a model response: the single step that owns
//! parse + repair + partial salvage + the retry policy.
//!
//! Review, streaming review and scan all go through [`request_findings`], so
//! they fail (and recover) identically:
//!
//! 1. **Empty-content recovery (#536)** — a reasoning model may burn the whole
//!    budget on chain-of-thought and return nothing. If `finish_reason` is
//!    `length`, retry with a doubled budget (capped); otherwise salvage JSON
//!    from `reasoning_content`; otherwise fail explicitly.
//! 2. **Parse** — [`parse_findings`]: strict, then truncation repair, then
//!    salvage of every complete object. This is pure and has no transport.
//! 3. **Stricter-prompt retry** — if nothing could be parsed, ask once more
//!    with a stricter prompt, and parse that with the same function.
//!
//! The model is reached only through [`Transport`], so tests drive all of the
//! above with a scripted fake and no network.

use serde_json::Value;
use tracing::debug;

use super::LlmEvents;
use super::prompts::strict_retry_prompt;
use super::repair::{
    extract_json_and_summary, extract_partial_json_objects, repair_json_string,
    repair_truncated_json, strip_code_fences,
};
use super::transport::{Completion, Transport, Turn, Usage, preview_raw};
use crate::engine::types::ReviewIssue;
use crate::error::CoraError;

/// Cap for the empty-content budget escalation (#536).
const MAX_TOKENS_CEILING: u32 = 32_768;

/// Next output budget when a response came back with empty content.
/// `finish_reason == "length"` means reasoning consumed the budget — double
/// it, capped at [`MAX_TOKENS_CEILING`]. Any other reason → give up (None).
fn next_budget_on_empty(finish_reason: Option<&str>, current: u32) -> Option<u32> {
    if finish_reason != Some("length") {
        return None;
    }
    let doubled = current.saturating_mul(2);
    (doubled <= MAX_TOKENS_CEILING).then_some(doubled)
}

/// Flatten a `reasoning_content` value (string or content-parts array) to text.
fn reasoning_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let joined: Vec<String> = parts
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(std::string::ToString::to_string)
                })
                .collect();
            (!joined.is_empty()).then(|| joined.join("\n"))
        }
        _ => None,
    }
}

/// Last-resort raw response when `content` is empty: some models write the
/// final JSON inside their reasoning. Only accept when it plausibly contains
/// JSON — the parse layer still validates.
fn salvage_from_reasoning(reasoning: Option<&Value>) -> Option<String> {
    let text = reasoning_text(reasoning?)?;
    let trimmed = text.trim();
    let plausible =
        trimmed.starts_with('[') || trimmed.starts_with('{') || trimmed.contains("```json");
    plausible.then(|| trimmed.to_string())
}

/// Raw model text plus the usage and output budget that produced it.
#[derive(Debug)]
pub(crate) struct Recovered {
    pub content: String,
    pub usage: Option<Usage>,
    /// Budget of the attempt that finally produced content (>= the request's).
    pub max_tokens: u32,
}

fn add_usage(a: Option<Usage>, b: Option<Usage>) -> Option<Usage> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.plus(&b)),
        (a, b) => a.or(b),
    }
}

/// One logical completion: send the turn, and if the content comes back empty
/// raise the budget / salvage from reasoning instead of failing (#536).
/// Used by findings *and* by raw callers (commit messages).
pub(crate) async fn complete_with_recovery<T: Transport>(
    transport: &T,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<Recovered, CoraError> {
    let mut budget = max_tokens;
    let mut usage: Option<Usage> = None;
    loop {
        let Completion {
            content,
            finish_reason,
            reasoning,
            usage: attempt_usage,
        } = transport
            .complete(&Turn {
                system,
                user,
                max_tokens: budget,
            })
            .await?;
        usage = add_usage(usage, attempt_usage);

        if !content.trim().is_empty() {
            return Ok(Recovered {
                content,
                usage,
                max_tokens: budget,
            });
        }
        if let Some(next) = next_budget_on_empty(finish_reason.as_deref(), budget) {
            tracing::warn!(
                finish_reason = ?finish_reason,
                from = budget,
                to = next,
                "empty LLM content — retrying with raised max_tokens"
            );
            budget = next;
            continue;
        }
        if let Some(salvaged) = salvage_from_reasoning(reasoning.as_ref()) {
            tracing::warn!("content empty — salvaged JSON from reasoning_content");
            return Ok(Recovered {
                content: salvaged,
                usage,
                max_tokens: budget,
            });
        }
        return Err(CoraError::LlmParse(format!(
            "provider returned an EMPTY response (finish_reason={finish_reason:?}) \
             after raising max_tokens to {budget}. Raise `max_tokens` in config or disable \
             reasoning on the model."
        )));
    }
}

/// How much repair a parsed response needed. Anything other than `Clean`
/// means the model output was damaged and findings may be partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Recovery {
    Clean,
    /// Closed unterminated strings/brackets (truncated output).
    ClosedTruncation,
    /// Kept `recovered` complete objects, dropped `skipped` unparseable ones.
    PartialObjects {
        recovered: usize,
        skipped: usize,
    },
}

/// The interpreted result of a model response.
#[derive(Debug)]
pub(crate) struct Findings {
    pub issues: Vec<ReviewIssue>,
    /// Summary after the `|||` separator, `None` when absent/empty.
    pub summary: Option<String>,
    pub recovery: Recovery,
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

/// Interpret raw model text as findings: strict parse, then truncation
/// repair, then salvage of every complete object. Pure; no transport.
///
/// An error means *nothing usable* could be recovered — the caller's retry
/// policy keys off that.
pub(crate) fn parse_findings(raw: &str) -> Result<Findings, CoraError> {
    // Fast-fail when the response is clearly not JSON (empty body, provider
    // error page, rate-limit message, prose). Surfacing the raw prefix lets
    // users tell truncation from a provider error or HTML.
    if raw.trim().is_empty() {
        return Err(CoraError::LlmParse(format!(
            "provider returned an EMPTY response (no message content). Common cause: \
             reasoning consumed the output budget — raise `max_tokens` in config. {}",
            non_json_error_message(raw)
        )));
    }
    if !looks_like_json_array(raw) {
        return Err(CoraError::LlmParse(non_json_error_message(raw)));
    }

    let (json_str, summary) = extract_json_and_summary(raw);
    let json_str = strip_code_fences(&json_str);
    // Repair common LLM JSON mistakes (invalid escapes) before strict parse.
    let json_str = repair_json_string(&json_str);
    let summary = (!summary.is_empty()).then_some(summary);

    let strict_err = match serde_json::from_str::<Vec<ReviewIssue>>(&json_str) {
        Ok(issues) => {
            return Ok(Findings {
                issues,
                summary,
                recovery: Recovery::Clean,
            });
        }
        Err(e) => e.to_string(),
    };

    debug!(error = %strict_err, "strict parse failed, attempting JSON repair");
    let repaired = repair_truncated_json(&json_str);
    let repair_err = match serde_json::from_str::<Vec<ReviewIssue>>(&repaired) {
        Ok(issues) => {
            debug!("truncated JSON repair succeeded — some data may be partial");
            return Ok(Findings {
                issues,
                summary,
                recovery: Recovery::ClosedTruncation,
            });
        }
        Err(e) => e.to_string(),
    };

    // Last resort: every complete object that parses on its own. Recovers
    // findings that appeared before truncation/damage.
    debug!(error = %repair_err, "repair failed, trying partial object extraction");
    let partials = extract_partial_json_objects(&json_str);
    let total = partials.len();
    let issues: Vec<ReviewIssue> = partials
        .iter()
        .filter_map(|o| serde_json::from_str::<ReviewIssue>(o).ok())
        .collect();
    if !issues.is_empty() {
        let recovery = Recovery::PartialObjects {
            recovered: issues.len(),
            skipped: total - issues.len(),
        };
        debug!(
            ?recovery,
            "partial JSON object extraction recovered findings"
        );
        return Ok(Findings {
            issues,
            summary,
            recovery,
        });
    }

    let why = if total == 0 {
        "No complete JSON objects found in response.".to_string()
    } else {
        format!("Could not recover any valid objects from {total} partial objects.")
    };
    Err(CoraError::LlmParse(format!(
        "parse failed (original: {strict_err}, after repair: {repair_err}). {why} Raw response prefix: {}",
        preview_raw(raw)
    )))
}

/// Everything a findings request needs besides the transport.
pub(crate) struct FindingsRequest<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub max_tokens: u32,
}

/// Ask the model for findings under the one shared policy.
///
/// Returns the findings and the total usage across every attempt made
/// (including a failed first parse). Progress notices go to `events`;
/// nothing is printed here.
pub(crate) async fn request_findings<T: Transport>(
    transport: &T,
    events: &dyn LlmEvents,
    req: &FindingsRequest<'_>,
) -> Result<(Findings, Option<Usage>), CoraError> {
    let first = complete_with_recovery(transport, req.system, req.user, req.max_tokens).await?;
    let first_err = match parse_findings(&first.content) {
        Ok(f) => return Ok((log_recovery(f), first.usage)),
        Err(e) => e,
    };

    // The model produced nothing parseable — retry once with a stricter
    // prompt (same for stream and non-stream; same for review and scan).
    debug!(error = %first_err, "first parse attempt failed, retrying LLM request");
    events.status("Retrying (parse error)…");
    events.retry();
    let strict = strict_retry_prompt(req.user);
    let second = complete_with_recovery(transport, req.system, &strict, first.max_tokens).await?;
    let findings = log_recovery(parse_findings(&second.content)?);
    Ok((findings, add_usage(first.usage, second.usage)))
}

/// Damaged-but-salvaged output is accepted, but never silently.
fn log_recovery(f: Findings) -> Findings {
    if f.recovery != Recovery::Clean {
        tracing::warn!(recovery = ?f.recovery, findings = f.issues.len(), "model response needed repair; findings may be partial");
    }
    f
}

#[cfg(test)]
mod tests {
    use super::super::transport::usage_to_token_usage;
    use super::*;
    use crate::engine::types::{Severity, TokenUsage};

    /// Review-shaped view of [`parse_findings`] (summary as `""` when absent).
    #[allow(clippy::type_complexity)]
    fn parse_review_response(
        raw: &str,
        usage: Option<&Usage>,
    ) -> Result<(Vec<ReviewIssue>, String, Option<TokenUsage>), CoraError> {
        let f = parse_findings(raw)?;
        Ok((
            f.issues,
            f.summary.unwrap_or_default(),
            usage.map(usage_to_token_usage),
        ))
    }

    /// Scan-shaped view of [`parse_findings`] (summary as `Option`).
    #[allow(clippy::type_complexity)]
    fn parse_scan_response(
        raw: &str,
        usage: Option<&Usage>,
    ) -> Result<(Vec<ReviewIssue>, Option<String>, Option<TokenUsage>), CoraError> {
        let f = parse_findings(raw)?;
        Ok((f.issues, f.summary, usage.map(usage_to_token_usage)))
    }

    const SINGLE_ISSUE_JSON: &str = r#"[{"file":"src/main.rs","line":42,"severity":"critical","issue_type":"security","title":"SQL Injection","body":"User input is concatenated directly into SQL query.","suggested_fix":"Use parameterized queries."}]"#;

    const TWO_ISSUES_JSON: &str = r#"[
  {"file":"src/api.rs","line":10,"severity":"major","issue_type":"performance","title":"N+1 Query","body":"Query inside a loop.","suggested_fix":"Use eager loading."},
  {"file":"src/lib.rs","line":5,"severity":"minor","issue_type":"bugs","title":"Off-by-one","body":"Loop bound is off by one."}
]"#;

    const EMPTY_ARRAY: &str = "[]";

    #[test]
    fn budget_doubles_only_on_length() {
        assert_eq!(next_budget_on_empty(Some("length"), 4096), Some(8192));
        assert_eq!(next_budget_on_empty(Some("length"), 32768), None);
        assert_eq!(next_budget_on_empty(Some("stop"), 4096), None);
        assert_eq!(next_budget_on_empty(None, 4096), None);
    }

    #[test]
    fn salvage_accepts_only_jsonish_reasoning() {
        let arr = Value::String("[{\"file\":\"a.rs\"}]".to_string());
        assert!(salvage_from_reasoning(Some(&arr)).is_some());

        let fenced = Value::String("thinking... ```json\n[]\n```".to_string());
        assert!(salvage_from_reasoning(Some(&fenced)).is_some());

        let parts = Value::Array(vec![serde_json::json!({"text": "{\"x\":1}"})]);
        assert!(salvage_from_reasoning(Some(&parts)).is_some());

        let prose = Value::String("the diff looks fine overall".to_string());
        assert!(salvage_from_reasoning(Some(&prose)).is_none());
        assert!(salvage_from_reasoning(None).is_none());
    }

    #[test]
    fn empty_raw_is_explicit_not_eof() {
        let err = parse_review_response("", None).unwrap_err();
        assert!(err.to_string().contains("EMPTY"), "got: {err}");
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

    #[test]
    fn parse_response_with_invalid_escapes() {
        // End-to-end: LLM response with invalid escapes should parse successfully
        let raw = r#"[{"file":"src/main.rs","line":10,"severity":"critical","issue_type":"security","title":"SQL Injection","body":"query ends with \n no wait \\","suggested_fix":"Use params"}]"#;
        let result = parse_review_response(raw, None).unwrap();
        assert_eq!(result.0.len(), 1);
        assert_eq!(result.0[0].title, "SQL Injection");
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

    // ─── Fake transport: the policy, driven end to end with no network ───

    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::super::NoEvents;
    use super::super::transport::SseAccumulator;

    /// Scripted transport: pops one canned completion per call and records
    /// every turn it was asked to send.
    struct Scripted {
        replies: RefCell<VecDeque<Result<Completion, CoraError>>>,
        seen: RefCell<Vec<(String, u32)>>,
    }

    impl Scripted {
        fn new(replies: Vec<Result<Completion, CoraError>>) -> Self {
            Self {
                replies: RefCell::new(replies.into()),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    impl Transport for Scripted {
        async fn complete(&self, turn: &Turn<'_>) -> Result<Completion, CoraError> {
            self.seen
                .borrow_mut()
                .push((turn.user.to_string(), turn.max_tokens));
            self.replies
                .borrow_mut()
                .pop_front()
                .expect("scripted transport ran out of replies")
        }
    }

    /// Same scripted bodies, but delivered as SSE and decoded by the real
    /// [`SseAccumulator`] — the streaming transport minus the socket.
    struct SseReplay(Scripted);

    fn sse_body(content: &str, finish: &str) -> String {
        let mut out = String::new();
        // Split content into small deltas, including mid-string cuts.
        let chars: Vec<char> = content.chars().collect();
        for piece in chars.chunks(7) {
            let s: String = piece.iter().collect();
            let chunk = serde_json::json!({"choices":[{"delta":{"content": s}}]});
            out.push_str(&format!("data: {chunk}\n\n"));
        }
        let last = serde_json::json!({"choices":[{"delta":{},"finish_reason": finish}],
            "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
        out.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
        out
    }

    impl Transport for SseReplay {
        async fn complete(&self, turn: &Turn<'_>) -> Result<Completion, CoraError> {
            // Reuse Scripted for bookkeeping; its Completion carries the body
            // in `content` and finish reason in `finish_reason`.
            let c = self.0.complete(turn).await?;
            let body = sse_body(&c.content, c.finish_reason.as_deref().unwrap_or("stop"));
            let mut acc = SseAccumulator::default();
            // Feed in awkward 13-byte pieces to exercise line reassembly.
            for piece in body.as_bytes().chunks(13) {
                acc.push(&String::from_utf8_lossy(piece), &NoEvents)?;
            }
            acc.finish(&NoEvents)
        }
    }

    fn reply(content: &str, finish: &str) -> Result<Completion, CoraError> {
        Ok(Completion {
            content: content.to_string(),
            finish_reason: Some(finish.to_string()),
            reasoning: None,
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        })
    }

    fn req(max_tokens: u32) -> FindingsRequest<'static> {
        FindingsRequest {
            system: "sys",
            user: "USER",
            max_tokens,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn malformed_first_response_retries_once_with_stricter_prompt() {
        let t = Scripted::new(vec![
            reply("sorry, here is prose", "stop"),
            reply(SINGLE_ISSUE_JSON, "stop"),
        ]);
        let (f, usage) = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        assert_eq!(t.calls(), 2);
        let seen = t.seen.borrow();
        assert_eq!(seen[0].0, "USER");
        assert!(seen[1].0.starts_with("USER"));
        assert!(seen[1].0.contains("MUST contain only valid JSON"));
        // Usage covers both attempts, not just the successful one.
        assert_eq!(usage.unwrap().total_tokens, 30);
    }

    #[test]
    fn stream_and_non_stream_follow_the_same_policy_and_result() {
        let script = || {
            vec![
                reply("not json at all", "stop"),
                reply(&format!("{SINGLE_ISSUE_JSON}|||sum"), "stop"),
            ]
        };
        let plain = Scripted::new(script());
        let streamed = SseReplay(Scripted::new(script()));

        let (a, ua) = block_on(request_findings(&plain, &NoEvents, &req(4096))).unwrap();
        let (b, ub) = block_on(request_findings(&streamed, &NoEvents, &req(4096))).unwrap();

        assert_eq!(a.issues.len(), b.issues.len());
        assert_eq!(a.issues[0].title, b.issues[0].title);
        assert_eq!(a.summary, b.summary);
        assert_eq!(a.summary.as_deref(), Some("sum"));
        // Same retry count and same retry prompt on both paths.
        assert_eq!(plain.calls(), 2);
        assert_eq!(streamed.0.calls(), 2);
        assert_eq!(plain.seen.borrow()[1].0, streamed.0.seen.borrow()[1].0);
        assert_eq!(ua.unwrap().total_tokens, ub.unwrap().total_tokens);
    }

    #[test]
    fn stream_retries_on_malformed_first_response() {
        // Regression: the streaming path used to have no retry at all.
        let streamed = SseReplay(Scripted::new(vec![
            reply("<html>502</html>", "stop"),
            reply(SINGLE_ISSUE_JSON, "stop"),
        ]));
        let (f, _) = block_on(request_findings(&streamed, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        assert_eq!(streamed.0.calls(), 2);
    }

    #[test]
    fn second_failure_is_surfaced_not_retried_again() {
        let t = Scripted::new(vec![reply("nope", "stop"), reply("still nope", "stop")]);
        let err = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        assert_eq!(t.calls(), 2);
    }

    #[test]
    fn transport_errors_are_not_retried_as_parse_failures() {
        let t = Scripted::new(vec![Err(CoraError::LlmStatus {
            status: 429,
            body: "slow down".into(),
        })]);
        let err = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap_err();
        assert!(matches!(err, CoraError::LlmStatus { status: 429, .. }));
        assert_eq!(t.calls(), 1);
    }

    #[test]
    fn truncated_response_is_repaired_without_a_retry() {
        let raw = r#"[{"file":"src/main.rs","line":42,"severity":"critical","issue_type":"security","title":"Hardcoded secret","body":"API key found","suggested_fix":"Use env vars"},{"file":"src/lib.rs","line":10,"severity":"major","issue_type":"bugs","title":"Unwrap panic","body":"incomplete"#;
        let t = Scripted::new(vec![reply(raw, "length")]);
        let (f, _) = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 2);
        assert_eq!(f.recovery, Recovery::ClosedTruncation);
        assert_eq!(t.calls(), 1);
    }

    #[test]
    fn closing_bracket_and_pipes_inside_a_body_do_not_truncate() {
        // From #573: `]` and `|||` inside a finding body must not split the JSON.
        let raw = r#"[{"file":"a.rs","line":1,"severity":"major","issue_type":"bugs","title":"T","body":"uses arr[0] and ] and ||| here"}]|||real summary"#;
        let t = Scripted::new(vec![reply(raw, "stop")]);
        let (f, _) = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        assert_eq!(f.issues[0].body, "uses arr[0] and ] and ||| here");
        assert_eq!(f.summary.as_deref(), Some("real summary"));
        assert_eq!(f.recovery, Recovery::Clean);
    }

    #[test]
    fn empty_content_with_length_doubles_the_budget() {
        let t = Scripted::new(vec![
            Ok(Completion {
                finish_reason: Some("length".into()),
                ..Completion::default()
            }),
            reply(SINGLE_ISSUE_JSON, "stop"),
        ]);
        let (f, _) = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        let seen = t.seen.borrow();
        assert_eq!(seen[0].1, 4096);
        assert_eq!(seen[1].1, 8192);
    }

    #[test]
    fn empty_content_with_length_doubles_the_budget_on_the_stream_too() {
        let streamed = SseReplay(Scripted::new(vec![
            reply("", "length"),
            reply(SINGLE_ISSUE_JSON, "stop"),
        ]));
        let (f, _) = block_on(request_findings(&streamed, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        let seen = streamed.0.seen.borrow();
        assert_eq!((seen[0].1, seen[1].1), (4096, 8192));
    }

    #[test]
    fn empty_content_at_ceiling_without_reasoning_is_an_explicit_error() {
        let t = Scripted::new(vec![Ok(Completion {
            finish_reason: Some("length".into()),
            ..Completion::default()
        })]);
        let err = block_on(request_findings(&t, &NoEvents, &req(MAX_TOKENS_CEILING))).unwrap_err();
        assert!(err.to_string().contains("EMPTY"), "{err}");
        assert_eq!(t.calls(), 1);
    }

    #[test]
    fn empty_content_salvages_json_from_reasoning() {
        let t = Scripted::new(vec![Ok(Completion {
            finish_reason: Some("stop".into()),
            reasoning: Some(Value::String(SINGLE_ISSUE_JSON.to_string())),
            ..Completion::default()
        })]);
        let (f, _) = block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        assert_eq!(f.issues.len(), 1);
        assert_eq!(t.calls(), 1);
    }

    #[test]
    fn retry_starts_from_the_escalated_budget() {
        let t = Scripted::new(vec![
            Ok(Completion {
                finish_reason: Some("length".into()),
                ..Completion::default()
            }),
            reply("prose", "stop"),
            reply(SINGLE_ISSUE_JSON, "stop"),
        ]);
        block_on(request_findings(&t, &NoEvents, &req(4096))).unwrap();
        let seen = t.seen.borrow();
        assert_eq!(
            seen.iter().map(|s| s.1).collect::<Vec<_>>(),
            [4096, 8192, 8192]
        );
    }

    #[test]
    fn scan_and_review_responses_share_one_parser() {
        // The same bytes yield the same findings whichever flow asked.
        let raw = format!("```json\n{TWO_ISSUES_JSON}\n```|||both");
        let review = parse_review_response(&raw, None).unwrap();
        let scan = parse_scan_response(&raw, None).unwrap();
        assert_eq!(review.0.len(), scan.0.len());
        assert_eq!(review.1, "both");
        assert_eq!(scan.1.as_deref(), Some("both"));
    }

    #[test]
    fn mid_array_garbage_keeps_the_good_objects() {
        let raw = r#"[{"file":"a.rs","line":1,"severity":"minor","title":"T","body":"B"}, {"file": 5, "oops"}, {"file":"c.rs","line":3,"severity":"info","title":"T","body":"B"}]"#;
        let f = parse_findings(raw).unwrap();
        assert_eq!(f.issues.len(), 2);
        assert!(matches!(
            f.recovery,
            Recovery::PartialObjects { recovered: 2, .. }
        ));
    }
}
