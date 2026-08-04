/// Built-in rules for the rule engine.
use crate::engine::Severity;
use crate::engine::rules::types::CustomRule;

/// Returns the full list of built-in rules.
pub fn builtin_rules() -> Vec<CustomRule> {
    vec![
        // --- Security ---
        CustomRule {
            id: "sec-hardcoded-secret".to_string(),
            pattern: r#"(?i)(?:password|api_?key|token|secret)\s*=\s*"[^"]+""#
                .to_string(),
            severity: Severity::Critical,
            message: "Possible hardcoded secret/credential detected. Use environment variables or a secrets manager.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec!["rules/".to_string(), "tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "sec-sql-concat".to_string(),
            pattern: r##"format!\("SELECT|f"SELECT|f"INSERT|f"UPDATE|f"DELETE|query\s*\+="##
                .to_string(),
            severity: Severity::Critical,
            message: "Possible SQL injection via string concatenation in query. Use parameterized queries.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec!["rules/".to_string(), "tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "sec-hardcoded-url".to_string(),
            pattern: r"http://[a-zA-Z0-9][\w.\-]+(:\d+)?(/\S*)?"
                .to_string(),
            severity: Severity::Major,
            message: "Insecure HTTP URL detected (not https). Use HTTPS for all external connections.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec!["rules/".to_string(), "tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "sec-tls-disabled".to_string(),
            pattern: r"tls_built_in_root_certs\(false\)|verify\s*=\s*False|InsecureRequestWarning|ACCEPT_INVALID_CERTS|dangerAcceptAnyServerCert".to_string(),
            severity: Severity::Critical,
            message: "TLS verification disabled. This allows man-in-the-middle attacks.".to_string(),
            languages: vec!["rs".to_string(), "py".to_string(), "go".to_string()],
            exclude: vec!["rules/".to_string(), "tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        // --- Bugs ---
        CustomRule {
            id: "bug-unwrap".to_string(),
            pattern: r"\.unwrap\(\)".to_string(),
            severity: Severity::Minor,
            message: "Use of `.unwrap()` can panic in production. Handle the error properly.".to_string(),
            languages: vec!["rs".to_string()],
            exclude: vec!["tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "bug-expect".to_string(),
            pattern: r#"\.expect\(""#
                .to_string(),
            severity: Severity::Minor,
            message: "Use of `.expect()` can panic in production. Consider proper error handling.".to_string(),
            languages: vec!["rs".to_string()],
            exclude: vec!["tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "bug-println".to_string(),
            pattern: r"(?:println!|dbg!|print!\s*\()".to_string(),
            severity: Severity::Minor,
            message: "Debug output macro found. Remove `println!`/`dbg!`/`print!` before merging.".to_string(),
            languages: vec!["rs".to_string()],
            exclude: vec!["tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "bug-todo".to_string(),
            pattern: r"(?i)\b(?:TODO|FIXME|HACK|XXX)\b".to_string(),
            severity: Severity::Info,
            message: "TODO/FIXME/HACK/XXX comment found. Consider resolving before merge.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec![],
            ..Default::default()
        },
        CustomRule {
            id: "bug-console-log".to_string(),
            pattern: r"console\.(?:log|debug|info)\s*\(".to_string(),
            severity: Severity::Minor,
            message: "Console logging statement found. Remove before merging to production.".to_string(),
            languages: vec!["js".to_string(), "ts".to_string()],
            exclude: vec!["tests/".to_string(), "test/".to_string()],
            ..Default::default()
        },
        CustomRule {
            id: "bug-hardcoded-port".to_string(),
            pattern: r#"(?::"8080"|:"3000"|:"5000")"#.to_string(),
            severity: Severity::Info,
            message: "Hardcoded port number detected. Consider using environment variables or config.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec![],
            ..Default::default()
        },
        // --- Quality ---
        CustomRule {
            id: "qual-error-ignore".to_string(),
            pattern: r"let\s+_\s*=\s*\w+::\w+\s*\(".to_string(),
            severity: Severity::Minor,
            message: "Error result discarded with `let _ =`. Consider handling the error explicitly.".to_string(),
            languages: vec!["all".to_string()],
            exclude: vec![],
            ..Default::default()
        },
        CustomRule {
            id: "qual-clone".to_string(),
            pattern: r"\.clone\(\)".to_string(),
            severity: Severity::Info,
            message: "Use of `.clone()` detected. Consider borrowing or ownership transfer.".to_string(),
            languages: vec!["rs".to_string()],
            exclude: vec![],
            ..Default::default()
        },
    ]
}

/// Post-match filter for rules that need additional validation after regex match.
/// Returns `true` to suppress a finding that the regex matched but should be ignored.
pub fn post_match_filter(rule_id: &str, line: &str) -> bool {
    match rule_id {
        "sec-hardcoded-secret" | "crypto/hardcoded-secret" => is_false_positive_secret(line),
        "sec-hardcoded-url" => is_false_positive_url(line),
        "config/cors-wildcard" => is_false_positive_cors(line),
        _ => false,
    }
}

/// Check if an `http://` URL match is a false positive.
///
/// Suppresses: XML/SVG namespaces, Docker internal hostnames, comment/docstring lines,
/// and other non-connection uses of `http://` URLs.
fn is_false_positive_url(line: &str) -> bool {
    let trimmed = line.trim();

    // Skip lines that are comments or docstrings
    let is_comment = trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("<!--")
        || trimmed.starts_with('*')
        || trimmed.starts_with("///")
        || trimmed.starts_with("//!")
        || trimmed.contains("\"\"\"") // Python/Rust docstrings
        || trimmed.contains("'''"  ); // Python docstrings
    if is_comment {
        return true;
    }

    let lower = line.to_lowercase();

    // XML/SVG namespace URIs are identifiers, not network connections
    if lower.contains("xmlns=") || lower.contains("xlink:href=") {
        return true;
    }

    // Loopback addresses
    if lower.contains("http://localhost")
        || lower.contains("http://127.0.0.1")
        || lower.contains("http://0.0.0.0")
        || lower.contains("http://[::1]")
    {
        return true;
    }

    // Docker internal hostnames (no TLS needed for container-to-container on same host)
    // Match http://<simple-hostname>:port pattern — no dots (not a public domain)
    static DOCKER_HOST_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)http://[a-z][\w-]*:\d+").unwrap());
    if DOCKER_HOST_RE.is_match(line) {
        // Only suppress if hostname has no dots (public domains have dots)
        if let Some(caps) = DOCKER_HOST_RE.captures(line) {
            let host = &caps[0];
            // Extract hostname part between "http://" and ":"
            if let Some(start) = host.find("//") {
                let host_part = &host[start + 2..];
                if let Some(colon) = host_part.find(':') {
                    let name = &host_part[..colon];
                    if !name.contains('.') {
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Check if a `hardcoded-secret` match is a false positive.
///
/// Suppresses: empty string values, variable references (no literal secret),
/// and UI binding patterns (Svelte $state, bind:value, form fields).
fn is_false_positive_secret(line: &str) -> bool {
    let lower = line.to_lowercase();

    // Empty string or empty-ish values: = ''  = ""  = $state('')
    if lower.contains("= ''") || lower.contains("= \"\"") || lower.contains("= $state('')") {
        return true;
    }

    // UI/framework binding patterns — variable names containing "secret"/"password"
    // that are form state, not actual credentials
    if lower.contains("$state(") || lower.contains("bind:") {
        return true;
    }

    // Object shorthand where the value is a variable reference, not a literal
    // e.g., { app_secret: formAppSecret } — RHS is a variable name, not a secret value
    // Variable references: no quotes, no digits mixed with special chars
    if let Some(colon_pos) = line.find(':') {
        let after_colon = line[colon_pos + 1..].trim();
        // If the RHS is a bare identifier (variable reference), it's not a hardcoded secret
        if !after_colon.is_empty()
            && !after_colon.starts_with('"')
            && !after_colon.starts_with('\'')
            && !after_colon.starts_with('`')
            && after_colon
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        {
            // Check that the rest is also identifier-like (no string literals)
            let is_bare_identifier = after_colon
                .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '$')
                .all(|part| {
                    part.is_empty()
                        || part
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '$')
                });
            if is_bare_identifier {
                return true;
            }
        }
    }

    false
}

/// Check if a CORS wildcard match is a false positive.
///
/// Suppresses: negation contexts ("no wildcard", "do not use *"), comments
/// documenting that wildcards are disallowed, and env var names that contain
/// "cors" but are not wildcard assignments.
fn is_false_positive_cors(line: &str) -> bool {
    let lower = line.to_lowercase();

    // Negation context — line says wildcards are NOT allowed.
    // Examples: "no wildcard", "do not use *", "without wildcard"
    let negation_markers = [
        "no wildcard",
        "no catch-all",
        "no catch all",
        "not.*wildcard",
        "do not.*wildcard",
        "do not.*\\*",
        "without.*wildcard",
        "never.*wildcard",
        "disallow.*wildcard",
        "prohibit.*wildcard",
        "avoid.*wildcard",
        "except.*wildcard",
    ];
    for marker in &negation_markers {
        if let Ok(re) = regex::Regex::new(marker) {
            if re.is_match(&lower) {
                return true;
            }
        }
    }

    // Env var or config key names containing "cors" — these are identifiers,
    // not wildcard assignments. e.g., TITEN_CORS_ORIGINS, CORS_ALLOWED_ORIGINS
    if lower.contains("cors_origins")
        || lower.contains("cors_allowed")
        || lower.contains("cors_config")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── sec-hardcoded-url false positive tests (issue #357, #364) ───

    #[test]
    fn url_svg_xmlns_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-url",
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">"#
        ));
    }

    #[test]
    fn url_xlink_href_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-url",
            r#"<use xlink:href="http://www.w3.org/1999/xlink">"#
        ));
    }

    #[test]
    fn url_docker_hostname_no_dots_is_false_positive() {
        assert!(post_match_filter("sec-hardcoded-url", "http://uteke:8767"));
        assert!(post_match_filter("sec-hardcoded-url", "http://redis:6379"));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "http://postgres-db:5432"
        ));
    }

    #[test]
    fn url_docker_hostname_with_dots_is_not_suppressed() {
        assert!(!post_match_filter(
            "sec-hardcoded-url",
            "http://example.com:8080"
        ));
        assert!(!post_match_filter(
            "sec-hardcoded-url",
            "http://api.staging.internal:3000"
        ));
    }

    #[test]
    fn url_in_comment_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "// Default: http://example.com:8080"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "# Server URL: http://127.0.0.1:8767"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "<!-- See http://www.w3.org/TR/ -->"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "/// Uses http://localhost for development"
        ));
    }

    #[test]
    fn url_in_docstring_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "    \"\"\"...default: http://127.0.0.1:8767...\"\"\""
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "    '''UTEKE_SERVER_URL — http://uteke:8767'''"
        ));
    }

    #[test]
    fn url_public_domain_is_real_finding() {
        assert!(!post_match_filter(
            "sec-hardcoded-url",
            "fetch('http://api.example.com/data')"
        ));
        assert!(!post_match_filter(
            "sec-hardcoded-url",
            "let url = 'http://evil.com/steal'"
        ));
    }

    #[test]
    fn url_loopback_still_suppressed() {
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "http://localhost:3000"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "http://127.0.0.1:5432"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-url",
            "http://0.0.0.0:8080"
        ));
    }

    #[test]
    fn unknown_rule_id_never_suppressed() {
        assert!(!post_match_filter("some-other-rule", "http://evil.com"));
    }

    // ─── crypto/hardcoded-secret false positive tests (issue #357) ───

    #[test]
    fn secret_empty_string_is_false_positive() {
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "let formAppSecret = $state('');"
        ));
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "let password = '';"
        ));
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "let api_key = \"\";"
        ));
    }

    #[test]
    fn secret_svelte_state_is_false_positive() {
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "let formPassword = $state('default12345678');"
        ));
    }

    #[test]
    fn secret_bind_is_false_positive() {
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "<input bind:value={formSecret} />"
        ));
    }

    #[test]
    fn secret_variable_reference_is_false_positive() {
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "...(formAppSecret && { app_secret: formAppSecret })"
        ));
    }

    #[test]
    fn secret_actual_hardcoded_is_real_finding() {
        assert!(!post_match_filter(
            "crypto/hardcoded-secret",
            "let password = supersecret12345"
        ));
        assert!(!post_match_filter(
            "crypto/hardcoded-secret",
            "const API_KEY = \"***\""
        ));
    }

    // ─── sec-hardcoded-secret (builtin rule ID) false positive tests ───

    #[test]
    fn builtin_rule_id_secret_empty_string_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-secret",
            "let formAppSecret = $state('');"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-secret",
            "let password = '';"
        ));
    }

    #[test]
    fn builtin_rule_id_secret_svelte_state_is_false_positive() {
        assert!(post_match_filter(
            "sec-hardcoded-secret",
            "let formPassword = $state('default12345678');"
        ));
    }

    #[test]
    fn builtin_rule_id_secret_actual_hardcoded_is_real_finding() {
        assert!(!post_match_filter(
            "sec-hardcoded-secret",
            "let password = supersecret12345"
        ));
    }

    // ─── config/cors-wildcard false positive tests (issue #483) ───

    #[test]
    fn cors_negation_no_wildcard_is_false_positive() {
        assert!(post_match_filter(
            "config/cors-wildcard",
            "// No wildcard — only explicit origins"
        ));
    }

    #[test]
    fn cors_negation_no_catch_all_is_false_positive() {
        assert!(post_match_filter(
            "config/cors-wildcard",
            "// No catch-all origin pattern is permitted"
        ));
    }

    #[test]
    fn cors_negation_do_not_use_wildcard_is_false_positive() {
        assert!(post_match_filter(
            "config/cors-wildcard",
            "# Do not use * in production"
        ));
    }

    #[test]
    fn cors_env_var_name_is_false_positive() {
        assert!(post_match_filter(
            "config/cors-wildcard",
            "TITEN_CORS_ORIGINS=https://example.com"
        ));
        assert!(post_match_filter(
            "config/cors-wildcard",
            "CORS_ALLOWED_ORIGINS=https://example.com"
        ));
    }

    #[test]
    fn cors_actual_wildcard_is_not_false_positive() {
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "Access-Control-Allow-Origin: *"
        ));
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "let origin = \"*\";"
        ));
    }

    #[test]
    fn cors_unrelated_rule_not_affected() {
        assert!(!post_match_filter(
            "crypto/hardcoded-secret",
            "No wildcard in this line"
        ));
    }
}
