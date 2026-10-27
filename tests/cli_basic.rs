use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::io::Write;
use std::process::Command;

// Binary name matches the [[bin]] in Cargo.toml
fn cora_cmd() -> Command {
    Command::cargo_bin("cora").unwrap()
}

// ═══════════════════════════════════════════════════════════
// CLI argument parsing tests
// ═══════════════════════════════════════════════════════════

#[test]
fn cli_version_returns_version_string() {
    cora_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn cli_help_shows_help() {
    cora_cmd()
        .arg("--help")
        .assert()
        .stdout(predicate::str::contains("AI Code Review"))
        .stdout(predicate::str::contains("Usage: cora"))
        .stdout(predicate::str::contains("Commands:"));
}

#[test]
fn cli_review_help_shows_review_help() {
    cora_cmd()
        .args(["review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Review a git diff"))
        .stdout(predicate::str::contains("--staged"))
        .stdout(predicate::str::contains("--unpushed"))
        .stdout(predicate::str::contains("--base"));
}

#[test]
fn cli_init_help_shows_init_help() {
    cora_cmd()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create a .cora.yaml"))
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn cli_scan_help_shows_scan_help() {
    cora_cmd()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Scan a project directory"))
        .stdout(predicate::str::contains("--include"))
        .stdout(predicate::str::contains("--exclude"));
}

#[test]
fn cli_auth_help_shows_auth_help() {
    cora_cmd()
        .args(["auth", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Manage API key"));
}

#[test]
fn cli_hook_help_shows_hook_help() {
    cora_cmd()
        .args(["hook", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pre-commit git hooks"));
}

#[test]
fn cli_providers_help_shows_providers_help() {
    cora_cmd()
        .args(["providers", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List detected"));
}

#[test]
fn cli_global_options_shown_in_help() {
    cora_cmd()
        .arg("--help")
        .assert()
        .stdout(predicate::str::contains("--config"))
        .stdout(predicate::str::contains("--format"))
        .stdout(predicate::str::contains("--provider"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--verbose"));
}

#[test]
fn cli_no_subcommand_fails() {
    // Running without a required subcommand shows usage and exits with error
    let output = cora_cmd().assert().failure();
    // The help message appears on stderr when no subcommand is given
    output.stderr(predicate::str::contains("Usage: cora"));
}

// ═══════════════════════════════════════════════════════════
// Config file loading tests (via CLI)
// ═══════════════════════════════════════════════════════════

#[test]
fn cli_accepts_config_flag() {
    // Just verify --config is accepted (it's a global flag)
    cora_cmd()
        .args(["--config", "/nonexistent/.cora.yaml", "--help"])
        .assert()
        .success();
}

#[test]
fn cli_accepts_format_flag() {
    cora_cmd()
        .args(["--format", "json", "--help"])
        .assert()
        .success();
}

#[test]
fn cli_accepts_provider_model_flags() {
    cora_cmd()
        .args(["--provider", "ollama", "--model", "llama3", "--help"])
        .assert()
        .success();
}

#[test]
fn init_creates_cora_yaml() {
    let tmp_dir = tempfile::tempdir().unwrap();

    cora_cmd()
        .args(["init"])
        .current_dir(tmp_dir.path())
        .assert()
        .success();

    let config_path = tmp_dir.path().join(".cora.yaml");
    assert!(config_path.exists(), ".cora.yaml should be created");

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("provider:"),
        "should contain provider section"
    );
}

#[test]
fn init_force_overwrites_existing() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let config_path = tmp_dir.path().join(".cora.yaml");

    // Create existing file
    let mut f = std::fs::File::create(&config_path).unwrap();
    writeln!(f, "# old config").unwrap();
    drop(f);

    cora_cmd()
        .args(["init", "--force"])
        .current_dir(tmp_dir.path())
        .assert()
        .success();

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !content.contains("# old config"),
        "old content should be overwritten"
    );
}

#[test]
fn init_without_force_fails_if_exists() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let config_path = tmp_dir.path().join(".cora.yaml");
    std::fs::write(&config_path, "# existing").unwrap();

    cora_cmd()
        .args(["init"])
        .current_dir(tmp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

// ═══════════════════════════════════════════════════════════
// Help points to agent mode, and never prints secrets from the environment
// ═══════════════════════════════════════════════════════════

#[test]
fn cli_mcp_help_explains_agent_as_reviewer() {
    cora_cmd()
        .args(["mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("agent-as-reviewer"))
        .stdout(predicate::str::contains("cora.review_prepare"))
        .stdout(predicate::str::contains("cora.scan_finalize"))
        .stdout(predicate::str::contains("claude mcp add cora"));
}

#[test]
fn cli_install_help_mentions_the_skill() {
    cora_cmd()
        .args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/cora-review"));
}

#[test]
fn cli_review_and_scan_help_point_to_no_llm_and_agent_mode() {
    for cmd in ["review", "scan"] {
        cora_cmd()
            .args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--no-llm"))
            .stdout(predicate::str::contains("cora mcp --help"));
    }
}

#[test]
fn cli_help_does_not_print_secret_env_values() {
    // clap prints `[env: NAME=value]` by default; a key in the environment
    // must never end up in help output (logs, screenshots, CI).
    for args in [
        &["--help"][..],
        &["review", "--help"],
        &["upload-sarif", "--help"],
    ] {
        cora_cmd()
            .args(args)
            .env("CORA_API_KEY", "sk-secret-do-not-print-123")
            .env("GITHUB_TOKEN", "ghp_secret_do_not_print_456")
            .assert()
            .stdout(predicate::str::contains("sk-secret-do-not-print-123").not())
            .stdout(predicate::str::contains("ghp_secret_do_not_print_456").not());
    }
}

#[test]
fn cli_help_does_not_print_url_or_deployment_env_values() {
    // A base URL can carry credentials (`user:pass@host`, `?key=...`), and the
    // model/provider names reveal internal deployments (#687).
    for args in [&["--help"][..], &["review", "--help"], &["scan", "--help"]] {
        cora_cmd()
            .args(args)
            .env(
                "CORA_BASE_URL",
                "https://u:hunter2pass@gateway.internal/v1?key=k3yvalue",
            )
            .env("CORA_MODEL", "internal-deployment-x")
            .env("CORA_PROVIDER", "internal-provider-y")
            .assert()
            .stdout(predicate::str::contains("hunter2pass").not())
            .stdout(predicate::str::contains("k3yvalue").not())
            .stdout(predicate::str::contains("gateway.internal").not())
            .stdout(predicate::str::contains("internal-deployment-x").not())
            .stdout(predicate::str::contains("internal-provider-y").not());
    }
}
