//! Regression tests for #545: `cora brain` in a fresh process must load
//! the on-disk vector index and emit the `vector` signal. Before the fix,
//! `VECTOR_CACHE` was only populated by `embed_project` (`cora index`), so
//! every search-only process silently degraded to FTS-only results.

use assert_cmd::prelude::*;
use std::path::PathBuf;
use std::process::Command;

fn cora_cmd() -> Command {
    Command::cargo_bin("cora").unwrap()
}

/// Isolated CODECORA_HOME + tiny project, so tests never touch real data.
/// Each call gets a unique temp dir (dropped with the returned guard), so two
/// concurrent `cargo test` runs cannot delete each other's sandbox.
fn sandbox(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("cora-545-{name}-"))
        .tempdir()
        .unwrap();
    let root = guard.path().to_path_buf();
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"p545\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("lib.rs"),
        "pub fn load_settings_file(path: &str) {}\npub fn send_request_to_server(url: &str) {}\n",
    )
    .unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    (guard, proj, home)
}

fn fixture_model() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-model")
}

fn run(cora: &mut Command) -> String {
    let out = cora.assert().success().get_output().stdout.clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn brain_fresh_process_emits_vector_signal() {
    let (_guard, proj, home) = sandbox("usearch");

    // 1. Index in one process (the code model, here the tiny fixture).
    run(cora_cmd()
        .args(["index"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home)
        .env("CORA_EMBED_MODEL_DIR", fixture_model()));

    let vectors = home.join("cora-code").join("vectors");
    let files: Vec<String> = std::fs::read_dir(&vectors)
        .expect("a vectors directory after `cora index`")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with(".potion-code-v2.vecq")),
        "one vecq file per project after `cora index`, got {files:?}"
    );

    // 2. Search in a FRESH process — vector signal must fire.
    let out = run(cora_cmd()
        .args(["brain", "open a folder on disk"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home)
        .env("CORA_EMBED_MODEL_DIR", fixture_model()));

    assert!(
        out.contains("vector"),
        "fresh-process brain search must emit the vector signal, got:\n{out}"
    );
}

/// `brain.vector_store` and `brain.vector_bits` were removed (#750): an old
/// config still loads, warns once, and the vectors are per-project vecq files.
#[test]
fn removed_vector_options_warn_and_do_not_break_indexing() {
    let (_guard, proj, home) = sandbox("old-config");

    std::fs::write(
        proj.join(".cora.yaml"),
        "brain:\n  vector_store: usearch\n  vector_bits: '6'\n",
    )
    .unwrap();

    let out = cora_cmd()
        .args(["index"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home)
        .env("CORA_EMBED_MODEL_DIR", fixture_model())
        .assert()
        .success()
        .get_output()
        .clone();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no longer has any effect"), "{err}");

    let db_dir = home.join("cora-code");
    assert!(!db_dir.join("cora_index.usearch").exists());
    assert!(db_dir.join("vectors").is_dir());
}
