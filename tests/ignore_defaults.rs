//! Dependencies, build output, minified files and lockfiles are out of scope by
//! default for review, scan and the index (#767), at any depth.

use std::path::Path;
use std::process::Command;

const LEAK: &str = "var token = \"AKIAABCDEFGHIJKLMNOP1234\"; var password = \"hunter2hunter2\";\n";

/// A git project whose real code is `src/lib.rs` and whose other files are
/// dependencies, build output, a minified file and lockfiles, each with a secret.
fn polluted() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(p)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "."]);
    std::fs::create_dir_all(p.join("src")).unwrap();
    std::fs::write(p.join("src/lib.rs"), "pub fn real() {}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    for f in [
        "node_modules/lodash/index.js",
        "packages/app/node_modules/dep/index.js",
        "vendor/lib/v.js",
        "dist/bundle.js",
        "build/out.js",
        "coverage/c.js",
        "src/app.min.js",
        "package-lock.json",
        "yarn.lock",
    ] {
        let path = p.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, LEAK).unwrap();
    }
    git(&["add", "-A", "-f"]);
    git(&["commit", "-q", "-m", "pollution"]);
    d
}

fn cora(dir: &Path, home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_cora"));
    c.current_dir(dir)
        .env("CODECORA_HOME", home)
        .env("CORA_EMBED_OFFLINE", "1");
    c
}

fn text(c: &mut Command) -> String {
    let out = c.output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn scan_and_index_only_see_the_project_code() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    let scan = text(cora(d.path(), home.path()).args(["scan", "--no-llm", "--format", "compact"]));
    for leaked in [
        "node_modules",
        "vendor/",
        "dist/",
        "build/",
        "coverage/",
        ".min.js",
        "lock",
    ] {
        assert!(
            !scan.contains(leaked),
            "{leaked} must be ignored by default: {scan}"
        );
    }
    assert!(
        scan.contains("Scanned 1 files"),
        "only src/lib.rs is scanned: {scan}"
    );

    let index = text(cora(d.path(), home.path()).arg("index"));
    // Cargo-less project: only src/lib.rs (and nothing from the ignored paths).
    assert!(index.contains("Indexed 1 symbols from 1 files"), "{index}");
}

#[test]
fn a_diff_touching_ignored_paths_is_not_reviewed_and_says_so() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    let out = text(cora(d.path(), home.path()).args(["review", "--base", "HEAD~1", "--no-llm"]));
    assert!(out.contains("skipped by the built-in ignores"), "{out}");
    assert!(
        out.contains("No changes to review"),
        "no finding about ignored files: {out}"
    );
    assert!(!out.contains("hardcoded"), "{out}");
}

#[test]
fn ignore_defaults_false_brings_them_back_and_user_patterns_still_add() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(".cora.yaml"), "ignore:\n  defaults: false\n").unwrap();
    let scan = text(cora(d.path(), home.path()).args(["scan", "--no-llm", "--format", "compact"]));
    assert!(scan.contains("vendor/lib/v.js"), "{scan}");
    assert!(
        scan.contains("packages/app/node_modules/dep/index.js"),
        "{scan}"
    );

    // defaults off + the user's own pattern: only theirs applies.
    std::fs::write(
        d.path().join(".cora.yaml"),
        "ignore:\n  defaults: false\n  files: [\"vendor/**\"]\n",
    )
    .unwrap();
    let scan = text(cora(d.path(), home.path()).args(["scan", "--no-llm", "--format", "compact"]));
    assert!(!scan.contains("vendor/lib/v.js"), "{scan}");
    assert!(scan.contains("dist/bundle.js"), "{scan}");
}
