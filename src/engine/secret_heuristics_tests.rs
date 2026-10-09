//! Table-driven, end-to-end tests for the hardcoded-secret heuristics (#610).
//!
//! Every row is one added line in a one-line diff, run through the real review
//! path: all deterministic scanners (`deterministic::run`: rule engine, secrets
//! scanner, security scanner), merged into issues, then `postprocess` (ignore
//! rules and inline `cora-ignore:` markers). A row says whether a hardcoded
//! secret finding survives. A failure prints every mismatching row.
//!
//! Rows that expose a real false positive/negative which is not fixed yet live
//! in [`KNOWN_GAPS`], asserting the CURRENT behaviour so a fix flips the test
//! and forces the row to move into [`CASES`].

use crate::config::schema::Config;
use crate::engine::deterministic;
use crate::engine::diff_parser::parse_diff;
use crate::engine::index_bridge::IndexBridge;
use crate::engine::postprocess::{Source, postprocess};

/// (file extension, added line, expect a hardcoded-secret finding)
type Row = (&'static str, &'static str, bool);

/// Is a hardcoded-secret finding reported for `line` in a `.{ext}` file?
fn flagged(ext: &str, line: &str) -> bool {
    let diff = format!(
        "diff --git a/src/app.{ext} b/src/app.{ext}\n--- a/src/app.{ext}\n+++ b/src/app.{ext}\n@@ -0,0 +1 @@\n+{line}\n"
    );
    let chunks = parse_diff(&diff);
    let report = deterministic::run(&chunks, &Config::default(), &IndexBridge::unavailable());
    let issues = report.merge_into(Vec::new());
    postprocess(issues, &Source::Diff(&chunks), &Config::default())
        .iter()
        .any(|i| {
            let id = i.rule_id.as_deref().unwrap_or("");
            id.contains("hardcoded-secret") || id.starts_with("secrets/")
        })
}

fn mismatches(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .filter(|(ext, line, expect)| flagged(ext, line) != *expect)
        .map(|(ext, line, expect)| {
            format!(
                "  ({ext:?}, {line:?}): expected flagged={expect}, got {}",
                !expect
            )
        })
        .collect()
}

const CASES: &[Row] = &[
    // ── js / ts: quotes ──
    ("js", r#"const password = "hunter2hunter2";"#, true),
    ("js", "const apiKey = 'abcd1234efgh5678';", true),
    ("js", "const token = `abcd1234efgh5678`;", true),
    ("ts", r#"const password: string = "hunter2hunter2";"#, true),
    (
        "ts",
        r#"  readonly secret: string = "hunter2hunter2";"#,
        true,
    ),
    ("ts", "  secret: string;", false),
    // ── js / ts: trailing comments, with and without a colon ──
    (
        "js",
        r#"const password = "hunter2hunter2"; // rotate later"#,
        true,
    ),
    (
        "js",
        r#"const password = "hunter2hunter2"; // note: rotate later"#,
        true,
    ),
    (
        "ts",
        r#"const password: string = "hunter2hunter2"; // TODO: move to vault"#,
        true,
    ),
    // ── cora-ignore markers ──
    (
        "js",
        r#"const password = "hunter2hunter2"; // cora-ignore: crypto/hardcoded-secret"#,
        false,
    ),
    (
        "js",
        r#"const password = "hunter2hunter2"; // cora-ignore: sec-hardcoded-secret"#,
        false,
    ),
    (
        "py",
        r#"password = "hunter2hunter2"  # cora-ignore: crypto/hardcoded-secret, sec-hardcoded-secret"#,
        false,
    ),
    // a bare marker (no rule list) never suppresses
    (
        "js",
        r#"const password = "hunter2hunter2"; // cora-ignore"#,
        true,
    ),
    // ── js / ts: object shorthand and variable references ──
    ("ts", "  { app_secret: formAppSecret }", false),
    ("ts", "const body = { password: formPassword };", false),
    // ── js / ts: empty strings and UI bindings ──
    ("js", r#"let password = "";"#, false),
    ("js", "let password = '';", false),
    ("ts", r#"let password = $state('');"#, false),
    (
        "svelte",
        "<input type=\"password\" bind:value={password} />",
        false,
    ),
    ("ts", "let secret = $state(initialSecretValue);", false),
    // ── placeholder / real-looking values ──
    ("js", r#"const password = "changeme-please-123";"#, true),
    // ── python ──
    ("py", r#"password = "hunter2hunter2""#, true),
    ("py", "api_key = 'abcd1234efgh5678'", true),
    ("py", r#"password: str = "hunter2hunter2""#, true),
    ("py", r#"password = "hunter2hunter2"  # note: temp"#, true),
    ("py", r#"password = """#, false),
    ("py", "password = ''", false),
    // ── rust ──
    ("rs", r#"let password = "hunter2hunter2";"#, true),
    ("rs", r#"let password: &str = "hunter2hunter2";"#, true),
    ("rs", r#"const API_KEY: &str = "abcd1234efgh5678";"#, true),
    (
        "rs",
        r#"let password = "hunter2hunter2"; // note: test only"#,
        true,
    ),
    (
        "rs",
        r#"let password = "hunter2hunter2"; // cora-ignore: crypto/hardcoded-secret"#,
        false,
    ),
    ("rs", r#"let password = "";"#, false),
    ("rs", "    pub password: String,", false),
    ("rs", "    api_key: extract_api_key.clone(),", false),
    // ── go ──
    ("go", r#"var password = "hunter2hunter2""#, true),
    ("go", "const token = `abcd1234efgh5678`", true),
    ("go", r#"password := os.Getenv("DB_PASSWORD")"#, false),
    ("go", r#"password := """#, false),
    // ── java / kotlin ──
    (
        "java",
        r#"private static final String PASSWORD = "hunter2hunter2";"#,
        true,
    ),
    ("java", r#"String apiKey = "abcd1234efgh5678";"#, true),
    ("java", r#"String password = "";"#, false),
    (
        "java",
        r#"String password = "hunter2hunter2"; // cora-ignore: crypto/hardcoded-secret"#,
        false,
    ),
    ("kt", r#"val password = "hunter2hunter2""#, true),
    ("kt", r#"val apiKey: String = "abcd1234efgh5678""#, true),
    ("kt", r#"val password = """#, false),
    // ── yaml / json ──
    ("yaml", r#"password: "hunter2hunter2""#, true),
    ("yaml", "api_key: 'abcd1234efgh5678'", true),
    ("yaml", "password: ${DB_PASSWORD}", false),
    ("yaml", "password: \"\"", false),
    ("json", r#"  "password": "","#, false),
    ("json", r#"  "password": "${DB_PASSWORD}","#, false),
    // ── env / .properties ──
    ("env", "DB_PASSWORD=hunter2hunter2", true),
    ("env", "API_KEY=abcd1234efgh5678", true),
    ("env", "DB_PASSWORD=", false),
    ("properties", "db.password=hunter2hunter2", true),
    ("properties", "db.password=", false),
    // ── shell ──
    ("sh", r#"export DB_PASSWORD="hunter2hunter2""#, true),
    ("sh", "API_KEY='abcd1234efgh5678'", true),
    ("sh", r#"password="""#, false),
    (
        "sh",
        r#"export DB_PASSWORD="hunter2hunter2" # note: dev"#,
        true,
    ),
    // ── sql ──
    (
        "sql",
        "ALTER ROLE app SET password = 'hunter2hunter2';",
        true,
    ),
    ("sql", "ALTER ROLE app SET password = '';", false),
];

/// Rows exposing a real false negative/positive that is not fixed yet. Each
/// asserts the CURRENT behaviour (`expect` = what the scanner does today, with
/// the ideal result in the trailing comment). Move a row into [`CASES`] with
/// the ideal expectation when its issue is fixed.
const KNOWN_GAPS: &[Row] = &[
    // KNOWN GAP #xxx - FP: env-var reads and call expressions on the RHS are reported as hardcoded secrets (ideal: not flagged)
    ("js", "const token = process.env.API_TOKEN;", true),
    (
        "js",
        "const secret = process.env.SESSION_SECRET || fallbackValue;",
        true,
    ),
    ("py", r#"password = os.environ["DB_PASSWORD"]"#, true),
    ("py", r#"password = os.getenv("DB_PASSWORD")"#, true),
    ("py", "password = get_password_from_vault()", true),
    ("rs", r#"let token = std::env::var("API_TOKEN")?;"#, true),
    (
        "rs",
        r#"let token = std::env::var("API_TOKEN").unwrap_or_default();"#,
        true,
    ),
    ("rs", r#"let password = String::new();"#, true),
    (
        "java",
        r#"String password = System.getenv("DB_PASSWORD");"#,
        true,
    ),
    ("kt", r#"val password = System.getenv("DB_PASSWORD")"#, true),
    // KNOWN GAP #xxx - FP: shell-style `${VAR}`/`$VAR` interpolation in env, .properties and shell files (ideal: not flagged)
    ("env", "DB_PASSWORD=${DB_PASSWORD_FROM_VAULT}", true),
    ("properties", "db.password=${DB_PASSWORD}", true),
    ("sh", r#"export DB_PASSWORD="$VAULT_DB_PASSWORD""#, true),
    // KNOWN GAP #xxx - FN: Go `:=` short declarations are never matched by the regexes (ideal: flagged)
    ("go", r#"password := "hunter2hunter2""#, false),
    ("go", r#"var apiKey string = "abcd1234efgh5678""#, false),
    ("go", r#"password := "hunter2hunter2" // note: temp"#, false),
    // KNOWN GAP #xxx - FN: unquoted YAML scalar is mistaken for object shorthand; quoted JSON keys never match (ideal: flagged)
    ("yaml", "password: hunter2hunter2", false),
    ("json", r#"  "password": "hunter2hunter2","#, false),
    ("json", r#"  "apiKey": "abcd1234efgh5678""#, false),
    // KNOWN GAP #xxx - FN: SQL `PASSWORD '<literal>'` (no `=`) is not matched (ideal: flagged)
    (
        "sql",
        "CREATE USER app WITH PASSWORD 'hunter2hunter2';",
        false,
    ),
];

#[test]
fn hardcoded_secret_table() {
    let bad = mismatches(CASES);
    assert!(
        bad.is_empty(),
        "{} of {} rows mismatched:\n{}",
        bad.len(),
        CASES.len(),
        bad.join("\n")
    );
}

#[test]
fn hardcoded_secret_known_gaps_keep_current_behaviour() {
    let bad = mismatches(KNOWN_GAPS);
    assert!(
        bad.is_empty(),
        "a KNOWN_GAPS row changed behaviour (fixed? move it into CASES):\n{}",
        bad.join("\n")
    );
}
