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
    // #618: short declarations and colon-free types
    ("go", r#"password := "hunter2hunter2""#, true),
    ("go", r#"var apiKey string = "abcd1234efgh5678""#, true),
    ("go", r#"password := "hunter2hunter2" // note: temp"#, true),
    (
        "go",
        r#"password := "hunter2hunter2" // cora-ignore: crypto/hardcoded-secret"#,
        false,
    ),
    ("go", r#"var apiKey string = """#, false),
    ("go", r#"var apiKey string = os.Getenv("API_KEY")"#, false),
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
    // #619: unquoted YAML scalars
    ("yaml", "password: hunter2hunter2", true),
    ("yaml", "  db_password: hunter2hunter2", true),
    ("yaml", "- token: abcd1234efgh5678", true),
    ("yaml", "password: hunter2hunter2 # prod db", true),
    ("yaml", "password: !secret db", false),
    ("yaml", "password: !vault db_password_1", false),
    ("yaml", "password: null", false),
    ("yaml", "password: ~", false),
    ("yaml", "password: required", false),
    ("yaml", "password: *dbpass1", false),
    ("yaml", "password: &dbpass1 hunter2", false),
    ("yaml", "password: $DB_PASSWORD_1", false),
    ("yaml", "password: \"${DB_PASSWORD}\"", false),
    ("yaml", "password_file: /run/secrets/db_password", false),
    ("yaml", "password: /run/secrets/db_password", false),
    ("yaml", "secretName: db-credentials-secret", false),
    ("ts", "  secret: Secret1Type", true),
    // #619: quoted JSON keys
    ("json", r#"  "password": "hunter2hunter2","#, true),
    ("json", r#"  "apiKey": "abcd1234efgh5678""#, true),
    ("json", r#"  "password": "hunter2hunter2" "#, true),
    ("json", r#"  "apiKey": "${API_KEY}""#, false),
    ("json", r#"  "password": "$DB_PASSWORD","#, false),
    ("json", r#"  "password": "{{ db_password }}","#, false),
    ("json", r#"  "password": null,"#, false),
    ("json", r#"  "password": "","#, false),
    ("json", r#"  "password": "${DB_PASSWORD}","#, false),
    // inline JSON: placeholders stay quiet, a real literal beside one is still flagged
    ("json", r#"{"password": "${X}"}"#, false),
    ("json", r#"{"password": "${X}", "apiKey": ""}"#, false),
    ("json", r#"{"password": "", "token": "${T}"}"#, false),
    (
        "json",
        r#"{"password": "${X}", "token": "hunter2hunter2xx"}"#,
        true,
    ),
    ("json", r#"{"password": "hunter2hunter2xx"}"#, true),
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
    // #620: PASSWORD '<literal>' without `=`
    (
        "sql",
        "CREATE USER app WITH PASSWORD 'hunter2hunter2';",
        true,
    ),
    ("sql", "ALTER USER app PASSWORD 'hunter2hunter2';", true),
    (
        "sql",
        "CREATE USER app IDENTIFIED BY 'hunter2hunter2';",
        true,
    ),
    (
        "sql",
        "CREATE USER app WITH PASSWORD 'hunter2hunter2'; -- note: temp",
        true,
    ),
    ("sql", "CREATE USER app WITH PASSWORD '';", false),
    ("sql", "CREATE USER app WITH PASSWORD '%s';", false),
    ("sql", "CREATE USER app WITH PASSWORD '$1';", false),
    ("sql", "CREATE USER app WITH PASSWORD ':pw';", false),
    ("sql", "CREATE USER app WITH PASSWORD '{password}';", false),
    ("sql", "CREATE USER app IDENTIFIED BY '?';", false),
    ("sql", "CREATE USER app WITH PASSWORD NULL;", false),
    ("go", r#"q := "CREATE USER app WITH PASSWORD '%s'""#, false),
    (
        "py",
        r#"q = f"CREATE USER app WITH PASSWORD '{pw}'""#,
        false,
    ),
    // ── #616: env reads / call / member expressions / identifiers are not literals ──
    ("js", "const token = process.env.API_TOKEN;", false),
    (
        "js",
        "const secret = process.env.SESSION_SECRET || fallbackValue;",
        false,
    ),
    ("py", r#"password = os.environ["DB_PASSWORD"]"#, false),
    ("py", r#"password = os.getenv("DB_PASSWORD")"#, false),
    ("py", "password = get_password_from_vault()", false),
    ("rs", r#"let token = std::env::var("API_TOKEN")?;"#, false),
    (
        "rs",
        r#"let token = std::env::var("API_TOKEN").unwrap_or_default();"#,
        false,
    ),
    ("rs", r#"let password = String::new();"#, false),
    (
        "java",
        r#"String password = System.getenv("DB_PASSWORD");"#,
        false,
    ),
    (
        "kt",
        r#"val password = System.getenv("DB_PASSWORD")"#,
        false,
    ),
    ("js", "const password = userInput;", false),
    ("rs", "let token = cfg.token_value_here;", false),
    ("ts", "const apiKey: string = config.apiKeyValue;", false),
    ("rb", r#"password = ENV["DB_PASSWORD"]"#, false),
    ("php", r#"$password = $_ENV['DB_PASSWORD'];"#, false),
    (
        "cs",
        "var token = Environment.GetEnvironmentVariable(name);",
        false,
    ),
    // a quoted or concatenated literal in code is still a secret
    ("js", r#"password = "abc" + "defgh12345";"#, true),
    ("py", r#"password = b"hunter2hunter2xx""#, true),
    ("py", r#"password = f"hunter2hunter2xx""#, true),
    ("rs", r##"let password = r#"hunter2hunter2xx"#;"##, true),
    (
        "rs",
        r#"let password = String::from("hunter2hunter2xx");"#,
        true,
    ),
    (
        "rs",
        r#"let password = "hunter2hunter2xx".to_string();"#,
        true,
    ),
    // ── #617: shell-style interpolation is a reference, a real value is not ──
    ("env", "DB_PASSWORD=${DB_PASSWORD_FROM_VAULT}", false),
    ("properties", "db.password=${DB_PASSWORD}", false),
    ("sh", r#"export DB_PASSWORD="$VAULT_DB_PASSWORD""#, false),
    ("env", "DB_PASSWORD=$VAULT_DB_PASSWORD", false),
    (
        "sh",
        "export API_KEY=$(vault read -field=key secret/api)",
        false,
    ),
    (
        "sh",
        r#"export API_KEY="$(vault read -field=key secret/api)""#,
        false,
    ),
    (
        "sh",
        r#"export DB_PASSWORD="${DB_PASSWORD_FROM_VAULT}""#,
        false,
    ),
    ("ini", "password=${DB_PASSWORD}", false),
    ("yaml", "      - DB_PASSWORD=${DB_PASSWORD}", false),
    // real values in assignment-style files stay flagged
    ("env", "DB_PASSWORD=hunter2hunter2xx", true),
    ("sh", "export API_KEY=abcd1234efgh5678", true),
    ("properties", "password=hunter2hunter2xx", true),
    ("sh", r#"export X_SECRET="hunter2hunter2xx""#, true),
    ("env", "DB_PASSWORD=hunter2hunter2xx # prod", true),
    ("toml", "api_key = abcd1234efgh5678", true),
    ("ini", "password = hunter2hunter2xx", true),
    ("yaml", "      - DB_PASSWORD=hunter2hunter2xx", true),
    // a literal mixed into an interpolation is not a pure reference
    (
        "sh",
        r#"export DB_PASSWORD="${PREFIX}hunter2hunter2xx""#,
        true,
    ),
];

/// Rows exposing a real false negative/positive that is not fixed yet. Each
/// asserts the CURRENT behaviour (`expect` = what the scanner does today, with
/// the ideal result in the trailing comment). Move a row into [`CASES`] with
/// the ideal expectation when its issue is fixed.
const KNOWN_GAPS: &[Row] = &[
    // KNOWN GAP #628 - FN: an unquoted RHS in source code is treated as an expression, so a real literal default inside a call is missed (ideal: flagged)
    (
        "py",
        r#"password = os.environ.get("DB_PASSWORD", "hunter2hunter2xx")"#,
        false,
    ),
    (
        "rs",
        r#"let password = std::env::var("DB_PASSWORD").unwrap_or("hunter2hunter2xx".into());"#,
        false,
    ),
    (
        "js",
        r#"const password = process.env.DB_PASSWORD || "hunter2hunter2xx";"#,
        false,
    ),
    // KNOWN GAP #630 - FN: the scanner regex needs a >= 8 char token after `=`, so `new String("lit")` and other short-prefixed constructors are never matched (ideal: flagged)
    (
        "java",
        r#"String password = new String("hunter2hunter2xx");"#,
        false,
    ),
    // KNOWN GAP #629 - FN: shell `${VAR:-literal}` default with a real literal is treated as interpolation (ideal: flagged)
    ("sh", "DB_PASSWORD=${DB_PASSWORD:-hunter2hunter2xx}", false),
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
