//! End-to-end tests of the commit gate (#674) against real git repositories:
//! `cora hook verify`, `cora hook claude-guard`, and the installed pre-commit hook.
//! cora runs without any API key (deterministic judge), so nothing touches the network.

use assert_cmd::cargo::cargo_bin;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

struct Repo {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Repo {
    fn new() -> Self {
        let r = Repo {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        r.git(&["init", "-q"]);
        r.git(&["config", "user.email", "t@example.test"]);
        r.git(&["config", "user.name", "t"]);
        std::fs::write(r.dir.path().join("base.py"), "x = 1\n").unwrap();
        r.git(&["add", "-A"]);
        r.git(&["commit", "-q", "--no-verify", "-m", "init"]);
        r
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("HOME", self.home.path())
            .output()
            .unwrap()
    }

    /// A cora invocation with no API key and an isolated HOME.
    fn cora(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("cora"));
        c.args(args)
            .current_dir(self.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.home.path());
        c
    }

    fn stage(&self, file: &str, content: &str) {
        std::fs::write(self.path().join(file), content).unwrap();
        self.git(&["add", file]);
    }

    fn verify_code(&self) -> i32 {
        self.cora(&["hook", "verify", "--quiet"])
            .status()
            .unwrap()
            .code()
            .unwrap()
    }

    fn guard(&self, stdin_json: &str) -> (i32, String) {
        let mut child = self
            .cora(&["hook", "claude-guard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin_json.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

fn bash(command: &str) -> String {
    serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": command } }).to_string()
}

const CLEAN: &str = "def add(a, b):\n    return a + b\n";
const SECRET: &str = "password = \"hunter2hunter2\"\n";

#[test]
fn nothing_staged_is_nothing_to_gate() {
    let r = Repo::new();
    assert_eq!(r.verify_code(), 0);
}

#[test]
fn an_unreviewed_staged_diff_is_not_validated_until_it_is_reviewed() {
    let r = Repo::new();
    r.stage("util.py", CLEAN);
    assert_eq!(r.verify_code(), 11, "never reviewed");

    // A deterministic review of the staged diff records the verdict.
    let out = r
        .cora(&["review", "--staged", "--format", "compact"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(r.verify_code(), 0, "reviewed and clean");

    // Editing and re-staging invalidates it.
    r.stage("util.py", "def add(a, b):\n    return a - b\n");
    assert_eq!(r.verify_code(), 11, "edited since the review");
}

#[test]
fn a_blocking_verdict_blocks_the_gate() {
    let r = Repo::new();
    r.stage("app.py", SECRET);
    let _ = r
        .cora(&["review", "--staged", "--format", "compact"])
        .output()
        .unwrap();
    assert_eq!(r.verify_code(), 10, "the hardcoded secret blocks");
}

#[test]
fn the_claude_guard_denies_unreviewed_commits_and_allows_reviewed_ones() {
    let r = Repo::new();
    r.stage("util.py", CLEAN);

    let (code, why) = r.guard(&bash("git commit -m 'add util'"));
    assert_eq!(code, 2);
    assert!(why.contains("has not been reviewed"), "{why}");

    r.cora(&["review", "--staged", "--format", "compact"])
        .output()
        .unwrap();
    assert_eq!(r.guard(&bash("git commit -m 'add util'")).0, 0);
}

#[test]
fn the_claude_guard_never_allows_no_verify() {
    let r = Repo::new();
    r.stage("util.py", CLEAN);
    r.cora(&["review", "--staged", "--format", "compact"])
        .output()
        .unwrap();
    for cmd in [
        "git commit --no-verify -m x",
        "git commit -nm x",
        "git add . && git commit -n -m x",
    ] {
        let (code, why) = r.guard(&bash(cmd));
        assert_eq!(code, 2, "{cmd}");
        assert!(why.contains("not allowed"), "{why}");
    }
}

#[test]
fn the_claude_guard_blocks_a_reviewed_but_blocking_diff() {
    let r = Repo::new();
    r.stage("app.py", SECRET);
    r.cora(&["review", "--staged", "--format", "compact"])
        .output()
        .unwrap();
    let (code, why) = r.guard(&bash("git commit -m x"));
    assert_eq!(code, 2);
    assert!(why.contains("blocks the commit"), "{why}");
}

#[test]
fn the_claude_guard_leaves_everything_else_alone() {
    let r = Repo::new();
    r.stage("util.py", CLEAN); // unreviewed, but these are not commits
    assert_eq!(r.guard(&bash("git status")).0, 0);
    assert_eq!(r.guard(&bash("cargo test")).0, 0);
    assert_eq!(r.guard(&bash("git log --oneline")).0, 0);
    let other_tool = serde_json::json!({ "tool_name": "Edit", "tool_input": {} }).to_string();
    assert_eq!(r.guard(&other_tool).0, 0);
    assert_eq!(r.guard("not json at all").0, 0);
    assert_eq!(r.guard("").0, 0);
}

#[test]
fn the_installed_hook_blocks_a_secret_and_passes_clean_code() {
    let r = Repo::new();
    let cora = cargo_bin("cora");
    let install = r.cora(&["hook", "install"]).output().unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let hook = std::fs::read_to_string(r.path().join(".git/hooks/pre-commit")).unwrap();
    assert!(hook.contains("# cora-managed-hook") && hook.contains("hook verify --quiet"));

    let commit = |msg: &str| {
        Command::new("git")
            .args(["commit", "-q", "-m", msg])
            .current_dir(r.path())
            .env_clear()
            .env(
                "PATH",
                format!("/usr/bin:/bin:{}", cora.parent().unwrap().display()),
            )
            .env("HOME", r.home.path())
            .env("CORA_BIN", &cora)
            .output()
            .unwrap()
    };

    // A secret: the fallback review denies the commit.
    r.stage("app.py", SECRET);
    let denied = commit("add secret");
    assert!(
        !denied.status.success(),
        "{}",
        String::from_utf8_lossy(&denied.stdout)
    );
    let log = r.git(&["log", "--oneline"]);
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).lines().count(),
        1,
        "no commit was made"
    );

    // Clean code: the hook passes (the review it runs also records the verdict).
    r.git(&["reset", "-q"]);
    r.stage("util.py", CLEAN);
    let ok = commit("add util");
    assert!(
        ok.status.success(),
        "{}{}",
        String::from_utf8_lossy(&ok.stdout),
        String::from_utf8_lossy(&ok.stderr)
    );
    let log = r.git(&["log", "--oneline"]);
    assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 2);
}

#[test]
fn reinstalling_the_hook_upgrades_it_instead_of_wrapping_it_again() {
    let r = Repo::new();
    // A v1 hook, as written before the sentinel existed.
    let v1 = "#!/usr/bin/env bash\n# cora pre-commit hook — installed by `cora hook install`\ncora review --staged --format compact\n";
    let path = r.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, v1).unwrap();
    assert!(
        r.cora(&["hook", "install"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after.matches("hook verify --quiet").count(),
        1,
        "one hook, not two"
    );
    assert!(
        !after.contains("cora review --staged --format compact\n#"),
        "old body is gone"
    );
    assert!(
        !r.path().join(".git/hooks/pre-commit.pre-cora.bak").exists(),
        "no wrapping backup"
    );
}

/// Drive `cora mcp` over stdio: initialize, then one tools/call. Returns the
/// text of the tool result.
fn mcp_call(r: &Repo, tool: &str, arguments: serde_json::Value) -> String {
    let msgs = [
        serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
    ];
    let mut child = r
        .cora(&["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for m in &msgs {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let last = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rfind(|l| l.contains("\"id\":1"))
        .expect("tool response")
        .to_string();
    let v: serde_json::Value = serde_json::from_str(&last).unwrap();
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

fn staged_diff(r: &Repo) -> String {
    String::from_utf8_lossy(&r.git(&["diff", "--cached"]).stdout).to_string()
}

#[test]
fn an_agent_verdict_validates_the_exact_staged_diff_for_hook_and_guard() {
    let r = Repo::new();
    r.stage("util.py", CLEAN);
    assert_eq!(r.verify_code(), 11);
    assert_eq!(r.guard(&bash("git commit -m x")).0, 2);

    // The agent judges (no findings of its own) and finalizes with the diff.
    let text = mcp_call(
        &r,
        "cora.review_finalize",
        serde_json::json!({
            "diff": staged_diff(&r), "issues": []
        }),
    );
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["commit_gate"]["recorded"], true, "{text}");
    assert_eq!(v["commit_gate"]["blocking"], false);

    assert_eq!(r.verify_code(), 0, "validated by the agent");
    assert_eq!(r.guard(&bash("git commit -m x")).0, 0);

    // A different staged change is not covered by that verdict.
    r.stage("other.py", "y = 2\n");
    assert_eq!(r.verify_code(), 11);
    assert_eq!(r.guard(&bash("git commit -m x")).0, 2);
}

#[test]
fn an_agent_verdict_with_a_blocking_finding_blocks_the_commit() {
    let r = Repo::new();
    r.stage("util.py", CLEAN);
    let text = mcp_call(
        &r,
        "cora.review_finalize",
        serde_json::json!({
            "diff": staged_diff(&r),
            "issues": [{"file":"util.py","line":2,"severity":"critical","title":"bad","body":"b"}]
        }),
    );
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["commit_gate"]["blocking"], true, "{text}");
    assert_eq!(r.verify_code(), 10);
    assert_eq!(r.guard(&bash("git commit -m x")).0, 2);
}

#[test]
fn the_hook_accepts_what_the_agent_already_validated_without_a_second_review() {
    let r = Repo::new();
    let cora = cargo_bin("cora");
    assert!(
        r.cora(&["hook", "install"])
            .output()
            .unwrap()
            .status
            .success()
    );
    r.stage("util.py", CLEAN);
    mcp_call(
        &r,
        "cora.review_finalize",
        serde_json::json!({"diff": staged_diff(&r), "issues": []}),
    );
    let out = Command::new("git")
        .args(["commit", "-q", "-m", "add util"])
        .current_dir(r.path())
        .env_clear()
        // No `cora review` could run: only the verify step is reachable.
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", r.home.path())
        .env("CORA_BIN", &cora)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // git forwards a hook's stdout to its own stderr.
    assert!(String::from_utf8_lossy(&out.stderr).contains("already validated"));
}
