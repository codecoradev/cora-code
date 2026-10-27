//! `bug-println` treats every module of a binary-only package as program code,
//! while a library package keeps the rule (#682).
use std::path::Path;
use std::process::Command;

fn layout(root: &Path) {
    for (file, body) in [
        ("crates/cli/src/main.rs", "mod commands;\nfn main() {}\n"),
        (
            "crates/cli/src/commands.rs",
            "pub fn run() { println!(\"out\"); }\n",
        ),
        (
            "crates/core/src/lib.rs",
            "pub fn run() { println!(\"leak\"); }\n",
        ),
        (
            "crates/core/src/engine.rs",
            "pub fn go() { eprintln!(\"diag\"); }\n",
        ),
    ] {
        let p = root.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

fn scan(cwd: &Path, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_cora"))
        .args(["scan", "--no-llm", "--format", "compact"])
        .args(extra)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", cwd)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

fn assert_expected(text: &str) {
    assert!(
        text.contains("crates/core/src/lib.rs"),
        "library println must fire: {text}"
    );
    assert!(
        !text.contains("crates/cli/src/commands.rs"),
        "binary module must not: {text}"
    );
    assert!(
        !text.contains("crates/core/src/engine.rs"),
        "eprintln is never flagged: {text}"
    );
}

#[test]
fn println_is_flagged_in_a_library_package_but_not_in_a_binary_only_one() {
    let d = tempfile::tempdir().unwrap();
    layout(d.path());
    assert_expected(&scan(d.path(), &[]));
}

#[test]
fn the_package_probe_uses_the_scan_root_not_the_working_directory() {
    // `cora scan --path proj` from the parent: paths are relative to `proj`.
    let d = tempfile::tempdir().unwrap();
    layout(&d.path().join("proj"));
    assert_expected(&scan(d.path(), &["--path", "proj"]));
}
