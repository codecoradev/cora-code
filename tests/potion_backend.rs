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

/// Names of the per-project vector files.
fn vector_files(home: &Path) -> Vec<String> {
    std::fs::read_dir(home.join("cora-code").join("vectors"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default()
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

    assert!(
        vector_files(&home)
            .iter()
            .any(|f| f.ends_with(".potion-code-v2.vecq")),
        "the code model has its own per-project file: {:?}",
        vector_files(&home)
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
fn without_the_model_there_are_no_vectors_and_the_model_arriving_builds_them() {
    let (_g, proj, home) = sandbox();

    // 1. Offline, no model: no vector file, keyword search still answers.
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_OFFLINE", "1"),
    );
    assert!(vector_files(&home).is_empty(), "{:?}", vector_files(&home));
    let out = stdout(
        cora(&proj, &home)
            .args(["brain", "decode_json_text", "--json"])
            .env("CORA_EMBED_OFFLINE", "1"),
    );
    let results: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(results[0]["name"], "decode_json_text", "{out}");

    // 2. The model becomes available; no source file changed.
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", fixture_model()),
    );
    let files = vector_files(&home);
    assert!(
        files.iter().any(|f| f.ends_with(".potion-code-v2.vecq")),
        "the model arriving must build vectors even when no file changed: {files:?}"
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
fn a_missing_model_turns_the_vector_signal_off_without_failing() {
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
    assert!(err.contains("vector signal is off"), "{err}");
    assert!(vector_files(&home).is_empty());
}

#[test]
fn off_disables_embeddings_and_removed_values_warn() {
    let (_g, proj, home) = sandbox();
    let model = fixture_model();

    std::fs::write(proj.join(".cora.yaml"), "brain:\n  embedding: off\n").unwrap();
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    assert!(vector_files(&home).is_empty(), "off must build no vectors");

    // A removed value still loads: it means auto, with one warning.
    std::fs::write(proj.join(".cora.yaml"), "brain:\n  embedding: hashing\n").unwrap();
    let out = cora(&proj, &home)
        .arg("index")
        .env("CORA_EMBED_MODEL_DIR", &model)
        .assert()
        .success()
        .get_output()
        .clone();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("was removed"), "{err}");
    assert!(
        vector_files(&home)
            .iter()
            .any(|f| f.ends_with(".potion-code-v2.vecq"))
    );
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

#[test]
fn re_indexing_leaves_no_vector_of_a_deleted_function() {
    let (_g, proj, home) = sandbox();
    let model = fixture_model();
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let q = ["brain", "run a table query on the database", "--json"];
    let before = stdout(
        cora(&proj, &home)
            .args(q)
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    assert!(before.contains("run_table_query"), "{before}");

    // delete the function and index again
    std::fs::write(
        proj.join("lib.rs"),
        "pub fn load_settings_file(path: &str) {}\npub fn decode_json_text(text: &str) {}\n",
    )
    .unwrap();
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    let after = stdout(
        cora(&proj, &home)
            .args(q)
            .env("CORA_EMBED_MODEL_DIR", &model),
    );
    assert!(!after.contains("run_table_query"), "{after}");
}

#[test]
fn the_former_shared_index_is_removed_after_the_first_index() {
    let (_g, proj, home) = sandbox();
    let data = home.join("cora-code");
    std::fs::create_dir_all(&data).unwrap();
    for f in [
        "cora_index.usearch",
        "cora_index.keys",
        "cora_index.potion-code-v2.usearch",
    ] {
        std::fs::write(data.join(f), b"old").unwrap();
    }
    stdout(
        cora(&proj, &home)
            .arg("index")
            .env("CORA_EMBED_MODEL_DIR", fixture_model()),
    );
    for f in [
        "cora_index.usearch",
        "cora_index.keys",
        "cora_index.potion-code-v2.usearch",
    ] {
        assert!(!data.join(f).exists(), "{f} should be gone");
    }
}
