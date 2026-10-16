//! `cora-ignore` markers must not fail silently (#679).
use std::process::Command;

fn scan(src: &str) -> String {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/a.rs"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cora"))
        .args(["scan", "--no-llm", "--format", "compact"])
        .current_dir(d.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", d.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

const LINE: &str =
    "pub const ENV_API_KEY: &str = \"TOLE_API_KEY\"; // cora-ignore: sec-hardcoded-secret";

#[test]
fn trailing_text_without_a_separator_warns_and_keeps_the_finding() {
    let text = scan(&format!("{LINE} (an env var name)\n"));
    assert!(text.contains("matched no finding"), "{text}");
    assert!(text.contains("sec-hardcoded-secret"), "{text}");
    assert!(text.contains("Found 1 issue"), "{text}");
}

#[test]
fn an_unknown_rule_warns() {
    let text =
        scan("pub const ENV_API_KEY: &str = \"TOLE_API_KEY\"; // cora-ignore: no-such-rule\n");
    assert!(
        text.contains("no-such-rule") && text.contains("matched no finding"),
        "{text}"
    );
}

#[test]
fn the_separator_suppresses_without_a_warning() {
    let text = scan(&format!("{LINE} -- env var name\n"));
    assert!(text.contains("Found 0 issues"), "{text}");
    assert!(!text.contains("matched no finding"), "{text}");
}
