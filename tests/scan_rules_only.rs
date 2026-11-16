//! The deterministic pass of `cora scan` (`--no-llm`, also `--rules-only`) needs
//! no model and makes no network call, and the same findings survive when the
//! model's batches fail (#722).
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Planted issues: a hardcoded secret, SQL built by concatenation.
fn plant(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/a.rs"),
        "fn a() { let password = \"hunter2hunter2_planted\"; }\n\
         fn q(id: &str) { let sql = format!(\"SELECT * FROM t WHERE id = {}\", id); }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/b.py"),
        "import os\ntoken = \"planted_value_for_the_test_0123456789\"\n",
    )
    .unwrap();
}

/// A server that counts the connections it gets and answers each with `status`.
fn counting_server(status: u16) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            seen.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Error\r\ncontent-length: 7\r\nconnection: close\r\n\r\nfailing"
            );
        }
    });
    (url, hits)
}

fn scan(cwd: &Path, flag: Option<&str>, base_url: &str, home: &Path) -> (String, i32) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cora"));
    cmd.args(["scan", "--format", "compact"]);
    if let Some(f) = flag {
        cmd.arg(f);
    }
    let out = cmd
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("CODECORA_HOME", home)
        .env("CORA_API_KEY", "dummy")
        .env("CORA_PROVIDER", "openai")
        .env("CORA_MODEL", "m")
        .env("CORA_BASE_URL", base_url)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
        out.status.code().unwrap_or(-1),
    )
}

fn planted_found(text: &str) -> bool {
    text.contains("src/a.rs:1")
        && text.contains("sec-sql-concat")
        && text.contains("src/b.py:2")
        && text.contains("sec-hardcoded-secret")
}

#[test]
fn rules_only_finds_the_planted_issues_and_never_connects() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    plant(dir.path());
    let (url, hits) = counting_server(500);
    // The key and the endpoint ARE configured; the flag must still keep it offline.
    for flag in ["--no-llm", "--rules-only"] {
        let (text, code) = scan(dir.path(), Some(flag), &url, home.path());
        assert_eq!(code, 0, "{flag}: {text}");
        assert!(planted_found(&text), "{flag}: {text}");
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "no connection in rules-only mode"
    );
}

#[test]
fn rule_findings_survive_every_model_batch_failing() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    plant(dir.path());
    let (url, hits) = counting_server(504);
    let (text, _) = scan(dir.path(), None, &url, home.path());
    // The model was asked (so this is not the offline path) and failed ...
    assert!(
        hits.load(Ordering::SeqCst) >= 1,
        "the model should be contacted: {text}"
    );
    // ... yet the deterministic findings are all there.
    assert!(planted_found(&text), "{text}");
}
