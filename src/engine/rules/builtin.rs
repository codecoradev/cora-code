/// Built-in rules for the rule engine.
use regex::Regex;
use std::sync::LazyLock;

use crate::engine::Severity;
use crate::engine::rules::types::CustomRule;

/// Returns the full list of built-in rules.
pub fn builtin_rules() -> Vec<CustomRule> {
    vec![
        // --- Security ---
        CustomRule {
            id: "sec-hardcoded-secret".to_string(),
            pattern: secret_literal_pattern(),
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

/// Regex source shared by the `sec-hardcoded-secret` rule and the LLM secret
/// cross-check in `postprocess`, so a shape detected here is never dropped
/// there as a false positive (#607, #618-#620). Alternatives:
///
/// 1. `name [: type] = "lit"` / `name := "lit"` (Go) / `"name": "lit"` (JSON
///    quoted key); the key may be quoted.
/// 2. Go `var name type = "lit"` (colon-free type between name and `=`).
/// 3. SQL `PASSWORD '<lit>'` / `IDENTIFIED BY '<lit>'` (no `=`).
pub(crate) fn secret_literal_pattern() -> String {
    let kw = r"(?:password|api_?key|token|secret)";
    let ty = r"[&*\w<>\[\].?|]+";
    let lit = r#"(?:"[^"]+"|'[^']+')"#;
    format!(
        r#"(?i){kw}["']?(?:\s*:\s*{ty})?\s*(?::=|=|["']\s*:)\s*{lit}|(?i){kw}["']?\s+{ty}\s*=\s*{lit}|{SQL_PASSWORD_PATTERN}"#
    )
}

/// SQL password literal without `=`: `... WITH PASSWORD 'x'`, `IDENTIFIED BY 'x'`.
const SQL_PASSWORD_PATTERN: &str = r"(?i)\b(?:password|identified\s+by)\s+'([^']+)'";

static SQL_PASSWORD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SQL_PASSWORD_PATTERN).expect("sql password regex must compile"));

static SECRET_LITERAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&secret_literal_pattern()).expect("hardcoded secret regex must compile")
});

/// Does `line` show a hardcoded secret literal the deterministic detectors
/// report? Used by the LLM cross-check so LLM findings on the same shapes are
/// kept. A SQL placeholder (`PASSWORD '%s'`) is not a literal.
pub(crate) fn has_secret_literal(line: &str) -> bool {
    if let Some(c) = SQL_PASSWORD_RE.captures(line)
        && is_sql_placeholder(&c[1])
    {
        return false;
    }
    if quoted_placeholder_value(line) {
        return false;
    }
    SECRET_LITERAL_RE.is_match(line) || plain_scalar_secret(strip_trailing_comment(line))
}

/// A SQL literal that is a bind/format placeholder, not a password:
/// `%s`, `%v`, `$1`, `?`, `:name`, `@name`, `{}` / `{pw}` / `${PW}`, `<password>`.
fn is_sql_placeholder(lit: &str) -> bool {
    let lit = lit.trim();
    lit.starts_with(['$', ':', '@', '?'])
        || lit.contains('%')
        || (lit.contains('{') && lit.contains('}'))
        || (lit.starts_with('<') && lit.ends_with('>'))
}

/// Value of a whole-line `key: value` entry (YAML, JSON, `- key: value`) whose
/// key ends in a secret word. The line must be nothing but the entry (an
/// optional `- ` bullet, an optional trailing comma), so object shorthand inside
/// code (`{ password: formPassword }`, `pub password: String,`) never qualifies.
fn secret_kv_value(line: &str) -> Option<&str> {
    static KV_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?i)^\s*(?:-\s+)?["']?[\w.\-]*(?:password|passwd|pwd|secret|api_?key|token)["']?\s*:\s+(\S.*?)\s*,?\s*$"#,
        )
        .expect("kv regex must compile")
    });
    KV_RE
        .captures(line)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

/// Unquoted YAML scalar that looks like a real secret (#619). Conservative:
/// a single token of >= 8 chars with a digit or symbol in it, so bare words
/// (`required`, `formPassword`, `vault_db_password`) stay "references". Never
/// a YAML tag/anchor/alias/block indicator, `${..}`/`$VAR`, a path, or a
/// keyword (`true`, `null`, ...).
fn plain_scalar_secret(line: &str) -> bool {
    let Some(v) = secret_kv_value(line) else {
        return false;
    };
    let lower = v.to_ascii_lowercase();
    v.len() >= 8
        && !v.contains(char::is_whitespace)
        && !v.starts_with([
            '"', '\'', '`', '$', '!', '&', '*', '|', '>', '%', '@', '{', '[', '/', '~', '<', '.',
            '?', '-',
        ])
        && !v.contains("://")
        && !v.contains(['{', '}', '(', ')', ';', '#'])
        && !matches!(
            lower.as_str(),
            "true" | "false" | "null" | "none" | "nil" | "yes" | "no"
        )
        && v.chars()
            .any(|c| c.is_ascii_digit() || "!@#%^&*+=".contains(c))
}

/// Quoted `key: "value"` entry whose value is empty or an interpolation /
/// template placeholder (`${X}`, `$X`, `{{ x }}`): a reference, not a secret.
fn quoted_placeholder_value(line: &str) -> bool {
    // Unanchored on purpose: inline JSON (`{"password": "${X}", "apiKey": ""}`)
    // and call arguments carry the pair in the middle of the line. Every quoted
    // secret pair on the line must be a placeholder to suppress the finding, so a
    // real literal next to a placeholder is still reported.
    static PAIR_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?i)["']?[\w.\-]*(?:password|passwd|pwd|secret|api_?key|token)["']?\s*:\s*("[^"]*"|'[^']*')"#,
        )
        .expect("pair regex must compile")
    });
    let mut seen = false;
    for cap in PAIR_RE.captures_iter(line) {
        let v = &cap[1];
        let inner = &v[1..v.len() - 1];
        let placeholder = inner.is_empty()
            || (inner.starts_with("${") && inner.ends_with('}'))
            || (inner.starts_with("{{") && inner.ends_with("}}"))
            || (inner.starts_with('$')
                && inner.len() > 1
                && inner[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if !placeholder {
            return false;
        }
        seen = true;
    }
    seen
}

/// Post-match filter for rules that need additional validation after regex match.
/// Returns `true` to suppress a finding that the regex matched but should be ignored.
pub fn post_match_filter(rule_id: &str, line: &str) -> bool {
    match rule_id {
        "sec-hardcoded-secret" | "crypto/hardcoded-secret" => is_false_positive_secret(line),
        "sec-hardcoded-url" => is_false_positive_url(line),
        "config/cors-wildcard" => is_false_positive_cors(line),
        "injection/sql-concat" => is_false_positive_sql_concat(line),
        "config/debug-enabled" => is_false_positive_debug(line),
        "injection/eval" => is_false_positive_eval(line),
        "crypto/weak-hash" => is_false_positive_weak_hash(line),
        "crypto/ssl-verify-disabled" => is_false_positive_ssl_verify(line),
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

/// Cut a trailing line comment (`//`, `#`, `--`) or block-comment start (`/*`)
/// that is outside string literals. `#` and `--` only count at the start of the
/// line or after whitespace so `a#b` / `x--` inside code are left alone.
fn strip_trailing_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => {
                let at_boundary = i == 0 || bytes[i - 1].is_ascii_whitespace();
                if c == b'"' || c == b'\'' || c == b'`' {
                    quote = Some(c);
                } else if (c == b'/' && matches!(bytes.get(i + 1), Some(b'/') | Some(b'*')))
                    || (c == b'#' && at_boundary)
                    || (c == b'-' && bytes.get(i + 1) == Some(&b'-') && at_boundary)
                {
                    return &line[..i];
                }
            }
        }
        i += 1;
    }
    line
}

/// Byte index of the first `:` that is outside string literals.
fn first_unquoted_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'\'' || c == b'`' {
                    quote = Some(c);
                } else if c == b':' {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Check if a `hardcoded-secret` match is a false positive.
///
/// Suppresses: empty string values, variable references (no literal secret),
/// and UI binding patterns (Svelte $state, bind:value, form fields).
fn is_false_positive_secret(line: &str) -> bool {
    // Judge the code only: a trailing comment ("// note: fix later") must not
    // turn a real secret into an "object shorthand" false positive (#603).
    let line = strip_trailing_comment(line);
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

    // SQL placeholders in format strings: PASSWORD '%s', IDENTIFIED BY '$1' (#620)
    if let Some(c) = SQL_PASSWORD_RE.captures(line)
        && is_sql_placeholder(&c[1])
    {
        return true;
    }

    // `"password": "${X}"` / `password: "{{ x }}"`: a reference, not a secret (#619)
    if quoted_placeholder_value(line) {
        return true;
    }

    // YAML alias / anchor / tag / path / `~` reference (`password: *dbpass`,
    // `password: /run/secrets/db`): points at another value, not a secret (#619)
    if secret_kv_value(line).is_some_and(|v| v.starts_with(['*', '&', '!', '/', '~'])) {
        return true;
    }

    // Unquoted YAML `password: hunter2hunter2` is a real secret, not shorthand (#619)
    if plain_scalar_secret(line) {
        return false;
    }

    // Object shorthand where the value is a variable reference, not a literal
    // e.g., { app_secret: formAppSecret } — RHS is a variable name, not a secret value
    // Variable references: no quotes, no digits mixed with special chars
    if let Some(colon_pos) = first_unquoted_colon(line) {
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
            // The rest must not contain a string literal. Type annotations followed
            // by a literal (`password: string = "..."`, `password: str = "..."`,
            // `let password: &str = "..."`) are real secrets, not shorthand (#607).
            let is_bare_identifier = !after_colon.contains(['"', '\'', '`']);
            if is_bare_identifier {
                return true;
            }
        }
    }

    false
}

/// Check if a `sql-concat` match is a false positive.
///
/// Suppresses: comment lines, string literal descriptions, and lines where
/// the "+" is not actually string concatenation (e.g., arithmetic).
fn is_false_positive_sql_concat(line: &str) -> bool {
    let trimmed = line.trim();

    // Comment lines — SQL keywords in comments are not injection
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("--")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
    {
        return true;
    }

    // Python/Rust docstrings
    if trimmed.contains("\"\"\"") || trimmed.contains("'''") {
        return true;
    }

    false
}

/// Check if a `debug-enabled` match is a false positive.
///
/// Suppresses: comment lines documenting debug config, argument parser
/// definitions, and environment variable references.
fn is_false_positive_debug(line: &str) -> bool {
    let trimmed = line.trim();

    // Comment lines
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("--")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
    {
        return true;
    }

    // Argument parser definitions (Python argparse, JS commander, etc.)
    let lower = line.to_lowercase();
    if lower.contains("add_argument")
        || lower.contains("addoption")
        || lower.contains("argument(")
        || lower.contains(".option(")
        || lower.contains("parser.")
    {
        return true;
    }

    // Environment variable references (DEBUG from env, not hardcoded)
    if lower.contains("env")
        && (lower.contains("getenv") || lower.contains("environ") || lower.contains("from_env"))
    {
        return true;
    }

    false
}

/// Check if an `eval` match is a false positive.
///
/// Suppresses: comment lines, documentation, `evaluate` (not `eval`),
/// and safe eval with literal expressions.
fn is_false_positive_eval(line: &str) -> bool {
    let trimmed = line.trim();

    // Comment lines
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
    {
        return true;
    }

    // Python/Rust docstrings
    if trimmed.contains("\"\"\"") || trimmed.contains("'''") {
        return true;
    }

    let lower = line.to_lowercase();

    // "evaluate" or "evaluation" is not "eval"
    if lower.contains("evaluate") || lower.contains("evaluation") {
        return true;
    }

    // Imports of eval from ast/json (safe parsing utilities)
    if lower.contains("ast.literal_eval") || lower.contains("json.") {
        return true;
    }

    false
}

/// Check if a `weak-hash` match is a false positive.
///
/// Suppresses: comment lines, documentation, and import statements
/// that merely reference the API without calling it.
fn is_false_positive_weak_hash(line: &str) -> bool {
    let trimmed = line.trim();

    // Comment lines
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
    {
        return true;
    }

    // Python/Rust docstrings
    if trimmed.contains("\"\"\"") || trimmed.contains("'''") {
        return true;
    }

    let lower = line.to_lowercase();

    // Import/use statements (Python, Rust use, JS import)
    if lower.contains("import ")
        || lower.contains("use ")
        || lower.contains("require(")
        || lower.contains("#include")
    {
        return true;
    }

    // Type annotations or trait bounds (Rust)
    if lower.contains("impl ") || lower.contains("fn ") || lower.contains("type ") {
        return true;
    }

    false
}

/// Check if an `ssl-verify-disabled` match is a false positive.
///
/// Suppresses: comment lines, documentation, config schema definitions,
/// and environment variable references.
fn is_false_positive_ssl_verify(line: &str) -> bool {
    let trimmed = line.trim();

    // Comment lines
    if trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
    {
        return true;
    }

    // Python/Rust docstrings
    if trimmed.contains("\"\"\"") || trimmed.contains("'''") {
        return true;
    }

    let lower = line.to_lowercase();

    // Environment variable references (verify from env config, not hardcoded)
    if lower.contains("env")
        && (lower.contains("getenv")
            || lower.contains("environ")
            || lower.contains("from_env")
            || lower.contains("process.env"))
    {
        return true;
    }

    // Config schema/validation definitions (e.g., TypeScript interfaces, Pydantic)
    if lower.contains("interface ")
        || lower.contains("schema")
        || lower.contains("field(")
        || lower.contains("default:")
    {
        return true;
    }

    // Negation patterns — "verify = True" or "do not disable"
    if lower.contains("verify") && lower.contains("true") && !lower.contains("false") {
        return true;
    }

    false
}

/// Negation markers that indicate a line is documenting that wildcards
/// are disallowed. Matched against the comment-stripped, lowercased line.
/// Plain strings use `contains`; patterns ending with `.*` use regex.
const CORS_NEGATION_MARKERS: &[&str] = &[
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

static CORS_NEGATION_RE: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    CORS_NEGATION_MARKERS
        .iter()
        .filter_map(|m| Regex::new(m).ok())
        .collect()
});

/// Strip comment prefix from a line for negation checking.
/// Handles: `//`, `#`, `--`, `/*`, `*` (block comment continuation).
fn strip_comment_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    for prefix in &["//", "#", "--", "/*", "*"] {
        if let Some(stripped) = trimmed.strip_prefix(prefix) {
            return stripped.trim_start();
        }
    }
    trimmed
}

/// Check if a CORS wildcard match is a false positive.
///
/// Suppresses: negation contexts ("no wildcard", "do not use *") only when
/// the negation appears in a **comment prefix** (not mixed with code), and
/// env var *read* patterns (`env::var("...")`, `getenv("...")`).
///
/// Does NOT suppress actual wildcard assignments like `CORS_CONFIG = "*"`
/// or code that appears after a comment on the same line.
fn is_false_positive_cors(line: &str) -> bool {
    let lower = line.to_lowercase();

    // Negation context — but ONLY in comment prefix, not mixed with code.
    // First strip the comment prefix, then check if the remaining text
    // is purely a negation statement (no assignment/code after it).
    let comment_body = strip_comment_prefix(line);
    let comment_lower = comment_body.to_lowercase();

    // Only apply negation filter if the original line was a comment
    // AND the comment body does NOT contain code indicators (=, ", ', wildcard *)
    // after the negation phrase. This prevents suppressing mixed lines like
    // `// no wildcard for now, but origin = "*"` where real code follows the comment.
    let is_comment = line.trim_start() != comment_body;
    if is_comment {
        // Check for code indicators in the comment body — if present, the line
        // contains actual code after the comment, so negation should NOT suppress.
        let has_code = comment_body.contains('=')
            || comment_body.contains("fn ")
            || comment_body.contains("let ")
            || comment_body.contains("const ");
        if !has_code {
            for re in CORS_NEGATION_RE.iter() {
                if re.is_match(&comment_lower) {
                    return true;
                }
            }
        }
    }

    // Env var READ patterns (not bare assignments).
    // e.g., env::var("TITEN_CORS_ORIGINS"), getenv("CORS_CONFIG")
    if lower.contains("env::var(") || lower.contains("getenv(") || lower.contains("os.environ") {
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

    // ─── #603: trailing comments / quoted colons must not hide real secrets ───

    #[test]
    fn secret_with_trailing_comment_containing_colon_is_real_finding() {
        for l in [
            "const api_password = \"hunter2hunter2xx\"; // note: fix later",
            "password = \"hunter2hunter2xx\"  # TODO: rotate",
            "const password = \"hunter2hunter2xx\"; // cora-ignore: crypto/hardcoded-secret",
            "password = \"hunter2hunter2xx\" /* see: docs */",
        ] {
            assert!(!post_match_filter("crypto/hardcoded-secret", l), "{l}");
        }
    }

    #[test]
    fn secret_with_colon_inside_string_is_real_finding() {
        assert!(!post_match_filter(
            "crypto/hardcoded-secret",
            "const password = \"user:hunter2hunter2\";"
        ));
    }

    #[test]
    fn object_shorthand_with_trailing_comment_is_still_false_positive() {
        assert!(post_match_filter(
            "crypto/hardcoded-secret",
            "{ app_secret: formAppSecret } // note: from the form"
        ));
        assert!(post_match_filter(
            "sec-hardcoded-secret",
            "{ app_secret: formAppSecret }"
        ));
    }

    #[test]
    fn comment_stripping_ignores_markers_inside_strings() {
        assert_eq!(
            strip_trailing_comment("a = \"x // y\"; // c"),
            "a = \"x // y\"; "
        );
        assert_eq!(
            strip_trailing_comment("url = 'http://x#y'"),
            "url = 'http://x#y'"
        );
        assert_eq!(strip_trailing_comment("x # c"), "x ");
    }

    // ─── #607: typed declarations with a literal are real secrets ───

    #[test]
    fn typed_declaration_with_literal_is_real_finding() {
        for l in [
            "const password: string = \"hunter2hunter2xx\";",
            "password: str = \"hunter2hunter2xx\"",
            "let password: &str = \"hunter2hunter2xx\";",
            "private val apiSecret: String = \"hunter2hunter2xx\"",
            "const password: string = 'hunter2hunter2xx'; // note: ok",
        ] {
            assert!(!post_match_filter("crypto/hardcoded-secret", l), "{l}");
            assert!(!post_match_filter("sec-hardcoded-secret", l), "{l}");
        }
    }

    #[test]
    fn shorthand_without_literal_stays_false_positive() {
        for l in [
            "{ app_secret: formAppSecret }",
            "...(formAppSecret && { app_secret: formAppSecret })",
            "{ password: input.password, secret: cfg.secret }",
        ] {
            assert!(post_match_filter("crypto/hardcoded-secret", l), "{l}");
        }
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
        // env::var() read pattern — should be suppressed
        assert!(post_match_filter(
            "config/cors-wildcard",
            "let val = env::var(\"TITEN_CORS_ORIGINS\").unwrap();"
        ));
        assert!(post_match_filter(
            "config/cors-wildcard",
            "let val = getenv(\"CORS_ALLOWED_ORIGINS\");"
        ));
        assert!(post_match_filter(
            "config/cors-wildcard",
            "os.environ.get(\"CORS_ORIGINS\")"
        ));
    }

    #[test]
    fn cors_config_assignment_is_not_false_positive() {
        // Issue #488: bare CORS_CONFIG = "*" is a REAL finding, not env var read
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "CORS_CONFIG = \"*\""
        ));
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "cors_origins = \"*\""
        ));
    }

    #[test]
    fn cors_mixed_comment_code_not_suppressed() {
        // Issue #488: code after a comment should NOT be suppressed by negation
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "// no wildcard for now, but origin = \"*\""
        ));
        assert!(!post_match_filter(
            "config/cors-wildcard",
            "# except for wildcard endpoints: cors = \"*\""
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
