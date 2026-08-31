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
fn sandbox(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("cora-545-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"p545\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("lib.rs"),
        "pub fn alpha_widget() {}\npub fn beta_gadget() {}\n",
    )
    .unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    (proj, home)
}

fn run(cora: &mut Command) -> String {
    let out = cora.assert().success().get_output().stdout.clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn brain_fresh_process_emits_vector_signal() {
    let (proj, home) = sandbox("usearch");

    // 1. Index in one process (default usearch backend).
    run(cora_cmd()
        .args(["index"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home));

    let db_dir = home.join("cora-code");
    assert!(
        db_dir.join("cora_index.usearch").exists(),
        "usearch index file should exist after `cora index`"
    );

    // 2. Search in a FRESH process — vector signal must fire.
    let out = run(cora_cmd()
        .args(["brain", "alpha"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home));

    assert!(
        out.contains("vector"),
        "fresh-process brain search must emit the vector signal, got:\n{out}"
    );
}

#[test]
fn brain_vecq_backend_uses_own_extension() {
    let (proj, home) = sandbox("vecq");

    std::fs::write(proj.join(".cora.yaml"), "brain:\n  vector_store: vecq\n").unwrap();

    run(cora_cmd()
        .args(["index"])
        .current_dir(&proj)
        .env("CODECORA_HOME", &home));

    let db_dir = home.join("cora-code");
    assert!(
        db_dir.join("cora_index.vecq").exists(),
        "vecq config must produce a .vecq index file (config wiring #543)"
    );
    assert!(
        !db_dir.join("cora_index.usearch").exists(),
        "vecq config must not create a usearch file"
    );
}
