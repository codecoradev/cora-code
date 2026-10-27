//! `ignore.rules` entries in object form silence a rule under some paths only (#680).
use std::process::Command;

fn scan(config: &str) -> String {
    let d = tempfile::tempdir().unwrap();
    for dir in ["crates/cli/src", "crates/core/src"] {
        std::fs::create_dir_all(d.path().join(dir)).unwrap();
    }
    std::fs::write(d.path().join(".cora.yaml"), config).unwrap();
    for file in ["crates/cli/src/out.rs", "crates/core/src/lib.rs"] {
        std::fs::write(d.path().join(file), "pub fn f() { println!(\"x\"); }\n").unwrap();
    }
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

#[test]
fn a_rule_is_silenced_in_one_subtree_and_still_fires_in_another() {
    let text = scan(
        "ignore:\n  rules:\n    - rule: bug-println\n      files: [\"crates/cli/**\"]\n      reason: stdout is the cli interface\n",
    );
    assert!(
        text.contains("crates/core/src/lib.rs"),
        "core must still fire: {text}"
    );
    assert!(
        !text.contains("crates/cli/src/out.rs"),
        "cli must be silenced: {text}"
    );
}

#[test]
fn the_plain_string_form_still_silences_everywhere() {
    let text = scan("ignore:\n  rules:\n    - bug-println\n");
    assert!(!text.contains("bug-println"), "{text}");
}
