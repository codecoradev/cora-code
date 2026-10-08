//! JSON repair toolbox for LLM output.
//!
//! Pure string functions, no I/O: split off the optional `|||` summary,
//! strip code fences, fix invalid escapes, close truncated JSON, and salvage
//! complete objects from a truncated array. The *policy* for when to apply
//! each step lives in [`super::findings`]; this module only provides the
//! mechanics.

use tracing::debug;

/// Byte offset just past the first complete JSON array/object in `s`
/// (which must start with `[` or `{`), tracking string literals and escapes.
/// Returns `None` if the value is unterminated.
pub(super) fn json_value_end(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract JSON and optional summary (after ||| separator).
pub(super) fn extract_json_and_summary(raw: &str) -> (String, String) {
    // Fast path: response starts with a JSON array. Find its end with a
    // string/escape-aware scan so `]` or `|||` inside a string value cannot
    // truncate the JSON.
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        if let Some(end) = json_value_end(trimmed) {
            let rest = trimmed[end..].trim();
            let summary = match rest.strip_prefix("|||") {
                Some(s) => s.trim(),
                None => rest,
            };
            return (trimmed[..end].to_string(), summary.to_string());
        }
    }
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
pub(super) fn repair_json_string(json_str: &str) -> String {
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
pub(super) fn repair_truncated_json(json: &str) -> String {
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
pub(super) fn extract_partial_json_objects(json: &str) -> Vec<String> {
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
pub(super) fn repair_invalid_escapes(input: &str) -> String {
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
pub(super) fn is_valid_json_escape(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
}

/// Strip ```json / ``` code fences from the response.
pub(super) fn strip_code_fences(s: &str) -> String {
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

    const SINGLE_ISSUE_JSON: &str = r#"[{"file":"src/main.rs","line":42,"severity":"critical","issue_type":"security","title":"SQL Injection","body":"User input is concatenated directly into SQL query.","suggested_fix":"Use parameterized queries."}]"#;

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
    fn extract_json_ignores_brackets_inside_strings() {
        let raw = r#"[{"file":"a.rs","body":"uses arr[0] and ] and \"]\" here"}]|||Summary"#;
        let (json, summary) = extract_json_and_summary(raw);
        assert_eq!(summary, "Summary");
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[test]
    fn extract_json_pipes_inside_string_do_not_split() {
        let raw = r#"[{"body":"a ||| b"}] trailing summary"#;
        let (json, summary) = extract_json_and_summary(raw);
        assert_eq!(json, r#"[{"body":"a ||| b"}]"#);
        assert_eq!(summary, "trailing summary");
    }

    #[test]
    fn extract_json_unterminated_falls_back() {
        let (json, summary) = extract_json_and_summary("[{\"a\":\"x");
        assert_eq!(json, "[{\"a\":\"x");
        assert!(summary.is_empty());
    }
}
