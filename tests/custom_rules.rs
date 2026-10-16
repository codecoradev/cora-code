//! `cora config validate` must not call a broken custom rule valid (#684, #678).
use std::process::Command;

fn cora(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_cora"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("CORA_API_KEY", "test-key-not-real")
        .output()
        .unwrap()
}

fn write(dir: &std::path::Path, name: &str, body: &str) {
    if let Some(p) = std::path::Path::new(name).parent() {
        std::fs::create_dir_all(dir.join(p)).unwrap();
    }
    std::fs::write(dir.join(name), body).unwrap();
}

#[test]
fn an_invalid_regex_fails_validation_and_names_the_rule() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "bad.yaml",
        "rules_engine:\n  custom:\n    - id: bad\n      pattern: \"(\"\n      severity: major\n      message: m\n      languages: [\"rs\"]\n      exclude: []\n",
    );
    let out = cora(d.path(), &["config", "validate", "--config", "bad.yaml"]);
    assert!(!out.status.success(), "must not exit 0");
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("bad") && text.contains("regex"), "{text}");
}

#[test]
fn a_config_copied_from_the_docs_loads_and_the_rule_fires() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        ".cora.yaml",
        "rules_engine:\n  custom:\n    - id: probe\n      pattern: \"fn a\"\n      severity: major\n      message: probe\n      languages: [\"rust\"]\n",
    );
    write(d.path(), "src/a.rs", "fn a() {}\n");
    let ok = cora(d.path(), &["config", "validate"]);
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let scan = cora(d.path(), &["scan", "--no-llm", "--format", "compact"]);
    let text =
        String::from_utf8_lossy(&scan.stdout).to_string() + &String::from_utf8_lossy(&scan.stderr);
    assert!(text.contains("probe"), "rule must fire: {text}");
}

#[test]
fn an_unknown_language_is_rejected_with_the_value() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        ".cora.yaml",
        "rules_engine:\n  custom:\n    - id: probe\n      pattern: \"x\"\n      severity: major\n      message: m\n      languages: [\"klingon\"]\n",
    );
    let out = cora(d.path(), &["config", "validate"]);
    assert!(!out.status.success());
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("klingon"), "{text}");
}

#[test]
fn the_dry_run_flags_a_rule_that_matches_nothing() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        ".cora.yaml",
        "rules_engine:\n  custom:\n    - id: hit\n      pattern: \"fn a\"\n      severity: major\n      message: m\n      languages: [\"rs\"]\n    - id: miss\n      pattern: \"never_present_zzz\"\n      severity: major\n      message: m\n      languages: [\"rs\"]\n",
    );
    write(d.path(), "src/a.rs", "fn a() {}\n");
    let out = cora(d.path(), &["config", "validate", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("hit: 1 lines"), "{text}");
    assert!(text.contains("miss: 0 matches"), "{text}");
}
