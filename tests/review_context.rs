//! The agent reviewer tools get the same code intelligence as `cora review` and
//! `cora scan`, from an index they bring up to date themselves, using the code
//! model (#765). A tiny Model2Vec fixture stands in for `potion-code-16M-v2`.

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture_model() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-model")
}

/// A project whose functions are about three topics, and a fresh data directory
/// (nobody ran `cora index`).
fn sandbox(config_yaml: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let guard = tempfile::Builder::new()
        .prefix("cora-ctx-")
        .tempdir()
        .unwrap();
    let proj = guard.path().join("proj");
    std::fs::create_dir_all(proj.join("src/old")).unwrap();
    std::fs::create_dir_all(proj.join("src/new")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/old/net.rs"),
        "pub fn post_http_request_to_url(url: &str) {}\n\
         pub fn open_socket_to_server(url: &str) {}\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/old/db.rs"),
        "pub fn select_row_from_table(sql: &str) {}\n\
         pub fn insert_row_into_db(sql: &str) {}\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/new/reply.rs"),
        "pub fn send_response_to_client(socket: &str) {}\n",
    )
    .unwrap();
    if let Some(y) = config_yaml {
        std::fs::write(proj.join(".cora.yaml"), y).unwrap();
    }
    let home = guard.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (guard, proj, home)
}

/// Call one MCP tool through `cora mcp` over stdio and return its decoded result.
fn call_tool(proj: &Path, home: &Path, tool: &str, args: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cora"))
        .arg("mcp")
        .current_dir(proj)
        .env("CODECORA_HOME", home)
        .env("CORA_EMBED_MODEL_DIR", fixture_model())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},
            "clientInfo":{"name":"test","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
               "params":{"name":tool,"arguments":args}}),
    ];
    {
        let stdin = child.stdin.as_mut().unwrap();
        for r in &requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let reply = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["id"] == 2)
        .unwrap_or_else(|| panic!("no reply to tools/call: {text}"));
    let body = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool text: {reply}"));
    serde_json::from_str(body).unwrap_or_else(|_| json!({ "text": body }))
}

const DIFF: &str = "diff --git a/src/new/reply.rs b/src/new/reply.rs\n\
new file mode 100644\n\
--- /dev/null\n\
+++ b/src/new/reply.rs\n\
@@ -0,0 +1 @@\n\
+pub fn send_response_to_client(socket: &str) {}\n";

#[test]
fn review_prepare_indexes_the_project_and_finds_related_code_by_meaning() {
    let (_g, proj, home) = sandbox(None);
    assert!(
        !home.join("cora-code").join("cora.db").exists(),
        "fresh data dir"
    );

    let packet = call_tool(&proj, &home, "cora.review_prepare", json!({ "diff": DIFF }));
    let prompt = packet["user_prompt"].as_str().unwrap();

    assert!(prompt.contains("Code Intelligence (Brain)"), "{prompt}");
    assert!(prompt.contains("Related Code (by meaning)"), "{prompt}");
    // No word of the changed function's name is in this one, only the model
    // links them (both are about the network).
    assert!(prompt.contains("post_http_request_to_url"), "{prompt}");
    // The index and the vectors were built by the call itself.
    assert!(home.join("cora-code").join("cora.db").exists());
    assert!(
        std::fs::read_dir(home.join("cora-code").join("vectors"))
            .unwrap()
            .flatten()
            .any(|e| e
                .file_name()
                .to_string_lossy()
                .ends_with(".potion-code-v2.vecq"))
    );
}

#[test]
fn scan_prepare_gives_the_judge_the_same_context() {
    let (_g, proj, home) = sandbox(None);
    let packet = call_tool(
        &proj,
        &home,
        "cora.scan_prepare",
        json!({ "batch": 0, "include": ["src/new/**"] }),
    );
    let prompt = packet["user_prompt"].as_str().unwrap();
    assert!(prompt.contains("Code Intelligence (Brain)"), "{prompt}");
    assert!(prompt.contains("Related Code (by meaning)"), "{prompt}");
    assert!(prompt.contains("post_http_request_to_url"), "{prompt}");
    assert!(
        prompt.contains("is similar to `send_response_to_client`"),
        "{prompt}"
    );
}

#[test]
fn auto_index_false_only_reads_an_existing_index() {
    let (_g, proj, home) = sandbox(Some("review:\n  context_chain:\n    auto_index: false\n"));
    let packet = call_tool(&proj, &home, "cora.review_prepare", json!({ "diff": DIFF }));
    let prompt = packet["user_prompt"].as_str().unwrap();
    assert!(!prompt.contains("Code Intelligence (Brain)"), "{prompt}");
    assert!(
        !home.join("cora-code").join("cora.db").exists(),
        "nothing was indexed"
    );
}
