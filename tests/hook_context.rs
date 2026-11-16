//! `cora hook context`: the code-search block an agent hook injects before the
//! model answers. A tiny Model2Vec fixture stands in for the real model.

use assert_cmd::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cora(proj: &Path, home: &Path) -> Command {
    let mut c = Command::cargo_bin("cora").unwrap();
    c.current_dir(proj).env("CODECORA_HOME", home).env(
        "CORA_EMBED_MODEL_DIR",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-model"),
    );
    c
}

fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::Builder::new()
        .prefix("cora-hook-")
        .tempdir()
        .unwrap();
    let proj = guard.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("lib.rs"),
        "pub fn load_settings_file(path: &str) {}\npub fn send_request_to_server(url: &str) {}\n",
    )
    .unwrap();
    let home = guard.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (guard, proj, home)
}

fn run(c: &mut Command, stdin: &str) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "hooks must always exit 0: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_code_question_gets_a_block_naming_the_symbol_and_the_tools() {
    let (_g, proj, home) = sandbox();
    cora(&proj, &home).arg("index").assert().success();

    let input =
        serde_json::json!({"prompt": "where do we open a folder on disk", "cwd": proj}).to_string();
    let out = run(
        cora(&proj, &home).args(["hook", "context", "--agent", "claude"]),
        &input,
    );
    assert!(out.contains("load_settings_file"), "{out}");
    assert!(out.contains("cora_find_impact"), "{out}");
}

#[test]
fn chit_chat_and_an_unindexed_project_inject_nothing_and_exit_zero() {
    let (_g, proj, home) = sandbox();
    // Not indexed yet.
    let input =
        serde_json::json!({"prompt": "where do we open a folder on disk", "cwd": proj}).to_string();
    assert!(
        run(
            cora(&proj, &home).args(["hook", "context", "--agent", "claude"]),
            &input
        )
        .trim()
        .is_empty()
    );

    cora(&proj, &home).arg("index").assert().success();
    let input = serde_json::json!({"prompt": "thanks!", "cwd": proj}).to_string();
    assert!(
        run(
            cora(&proj, &home).args(["hook", "context", "--agent", "claude"]),
            &input
        )
        .trim()
        .is_empty()
    );
    // Garbage on stdin never fails the agent.
    assert!(
        run(
            cora(&proj, &home).args(["hook", "context", "--agent", "claude"]),
            "not json"
        )
        .trim()
        .is_empty()
    );
}

#[test]
fn hermes_gets_its_json_context_envelope() {
    let (_g, proj, home) = sandbox();
    cora(&proj, &home).arg("index").assert().success();
    let input = serde_json::json!({"cwd": proj, "extra": {"user_message": "where do we open a folder on disk"}}).to_string();
    let out = run(
        cora(&proj, &home).args(["hook", "context", "--agent", "hermes"]),
        &input,
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert!(
        v["context"]
            .as_str()
            .unwrap()
            .contains("load_settings_file"),
        "{out}"
    );
}

#[test]
fn an_explicit_prompt_flag_works_without_stdin_for_pi_and_opencode() {
    let (_g, proj, home) = sandbox();
    cora(&proj, &home).arg("index").assert().success();
    let out = run(
        cora(&proj, &home).args([
            "hook",
            "context",
            "--prompt",
            "where do we send a network request",
        ]),
        "",
    );
    assert!(out.contains("send_request_to_server"), "{out}");
}
