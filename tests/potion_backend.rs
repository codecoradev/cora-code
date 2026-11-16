//! The code embedding model as the default vector backend, end to end, with a
//! tiny Model2Vec model from `tests/fixtures/tiny-model` standing in for
//! `potion-code-16M-v2` (`CORA_EMBED_MODEL_DIR`), so nothing is downloaded.

use assert_cmd::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn cora(proj: &Path, home: &Path) -> Command {
    let mut c = Command::cargo_bin("cora").unwrap();
    c.current_dir(proj).env("CODECORA_HOME", home);
    c
}

fn fixture_model() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-model")
}

/// A project whose functions are about four different topics.
fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::Builder::new()
        .prefix("cora-potion-")
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
        "pub fn load_settings_file(path: &str) {}\n\
         pub fn decode_json_text(text: &str) {}\n\
         pub fn send_request_to_server(url: &str) {}\n\
         pub fn run_table_query(sql: &str) {}\n",
    )
    .unwrap();
    let home = guard.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (guard, proj, home)
}

fn stdout(c: &mut Command) -> String {
    String::from_utf8(c.assert().success().get_output().stdout.clone()).unwrap()
}

#[test]
fn the_code_model_gets_its_own_index_and_answers_a_semantic_question() {
    let (_g, proj, home) = sandbox();
    let model = fixture_model();

    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );

    let data = home.join("cora-code");
    assert!(
        data.join("cora_index.potion-code-v2.usearch").exists(),
        "the code model must not share the hashing index file"
    );

    // No word of the question is in the function name: only the model links them.
    let out = stdout(
        cora(&proj, &home)
            .args(["brain", "open a folder on disk", "--json"])
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    let first = &results[0];
    assert_eq!(first["name"], "load_settings_file", "{out}");
    assert!(
        first["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "vector"),
        "{out}"
    );
}

#[test]
fn switching_the_model_re_embeds_without_any_file_change() {
    let (_g, proj, home) = sandbox();

    // 1. Offline: the hashing fallback fills the historical index file.
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_OFFLINE", "1"),
    );
    let data = home.join("cora-code");
    assert!(data.join("cora_index.usearch").exists());
    assert!(!data.join("cora_index.potion-code-v2.usearch").exists());

    // 2. The model becomes available; no source file changed.
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", fixture_model()),
    );
    assert!(
        data.join("cora_index.potion-code-v2.usearch").exists(),
        "a new model must re-embed even when no file changed"
    );

    let out = stdout(
        cora(&proj, &home)
            .args(["brain", "send a network request", "--json"])
            .env("CORA_EMBED_MODEL_DIR", fixture_model()),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(results[0]["name"], "send_request_to_server", "{out}");
}

#[test]
fn a_missing_model_degrades_to_hashing_without_failing() {
    let (_g, proj, home) = sandbox();
    let empty = home.join("no-model");
    std::fs::create_dir_all(&empty).unwrap();

    // CORA_EMBED_MODEL_DIR names a directory without the model: warn and go on.
    let out = cora(&proj, &home)
        .arg("index")
        .env("CORA_EMBED_MODEL_DIR", &empty)
        .assert()
        .success()
        .get_output()
        .clone();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not available"), "{err}");
    assert!(home.join("cora-code").join("cora_index.usearch").exists());
}

#[test]
fn explore_completes_a_question_with_matches_by_meaning() {
    let (_g, proj, home) = sandbox();
    let model = fixture_model();
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );

    // No word of the question is in a symbol name, so keyword search finds none.
    let out = stdout(
        cora(&proj, &home)
            .args(["explore", "open a folder on disk", "--json"])
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    let first = &results[0];
    assert_eq!(first["name"], "load_settings_file", "{out}");
    assert_eq!(first["via"], "semantic", "{out}");

    // A one-word query stays a plain name search.
    let out = stdout(
        cora(&proj, &home)
            .args(["explore", "decode", "--json"])
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        results
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["via"] == "keyword"),
        "{out}"
    );
}

#[test]
fn explore_no_semantic_keeps_the_keyword_search_only() {
    let (_g, proj, home) = sandbox();
    let model = fixture_model();
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );

    let out = stdout(
        cora(&proj, &home)
            .args([
                "explore",
                "open a folder on disk",
                "--no-semantic",
                "--json",
            ])
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(results.as_array().unwrap().is_empty(), "{out}");
}
