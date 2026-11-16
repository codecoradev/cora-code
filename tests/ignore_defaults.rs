//! Dependencies, build output, minified files and lockfiles are out of scope by
//! default for review, scan and the index (#767), at any depth.

use std::path::Path;
use std::process::Command;

/// A line the secret scanners flag. Built at run time so the repository's own
/// scanners do not flag this test file.
fn leak() -> String {
    ["var pass", "word = \"hunter2", "hunter2\";\n"].concat()
}

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
        "src/app.min.js",
        "package-lock.json",
        "yarn.lock",
    ] {
        let path = p.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, leak()).unwrap();
    }
    // Directories that often hold a project's OWN code under these names: never
    // ignored by default (coveragepy's `coverage/` package is its whole source).
    for f in [
        "build/script.js",
        "coverage/report.js",
        "src/venv/env.js",
        "out/tool.js",
    ] {
        let path = p.join(f);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, leak()).unwrap();
    }
    git(&["add", "-A", "-f"]);
    git(&["commit", "-q", "-m", "pollution"]);
    d
}

const OWN: [&str; 4] = [
    "build/script.js",
    "coverage/report.js",
    "src/venv/env.js",
    "out/tool.js",
];

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
fn scan_and_index_skip_dependencies_but_keep_the_project_own_code() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    let scan = text(cora(d.path(), home.path()).args(["scan", "--no-llm", "--format", "compact"]));
    for leaked in ["node_modules", "vendor/", "dist/", ".min.js", "lock"] {
        assert!(
            !scan.contains(leaked),
            "{leaked} must be ignored by default: {scan}"
        );
    }
    for own in OWN {
        assert!(
            scan.contains(own),
            "{own} is the project's own code and must be scanned: {scan}"
        );
    }

    let index = text(cora(d.path(), home.path()).arg("index"));
    // src/lib.rs plus the four own-code files (5); nothing from the ignored paths.
    assert!(index.contains("Indexed"), "{index}");
    assert!(index.contains("from 5 files"), "{index}");
}

#[test]
fn a_diff_skips_dependencies_with_a_note_and_still_reviews_own_code() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    let out = text(cora(d.path(), home.path()).args(["review", "--base", "HEAD~1", "--no-llm"]));
    assert!(out.contains("skipped by the built-in ignores"), "{out}");
    let findings: String = out.lines().filter(|l| l.contains("CRITICAL")).collect();
    for ignored in [
        "node_modules",
        "vendor/",
        "dist/",
        ".min.js",
        "package-lock.json",
    ] {
        assert!(
            !findings.contains(ignored),
            "{ignored} produced a finding: {out}"
        );
    }
    for own in OWN {
        assert!(out.contains(own), "{own} must be reviewed: {out}");
    }
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
    assert!(scan.contains("dist/bundle.js"), "{scan}");

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

#[test]
fn ignore_keep_turns_single_built_ins_off() {
    let d = polluted();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".cora.yaml"),
        "ignore:\n  keep: [\"**/vendor/**\", \"**/package-lock.json\"]\n",
    )
    .unwrap();
    let scan = text(cora(d.path(), home.path()).args(["scan", "--no-llm", "--format", "compact"]));
    assert!(scan.contains("vendor/lib/v.js"), "kept: {scan}");
    assert!(
        !scan.contains("packages/app/node_modules"),
        "still ignored: {scan}"
    );
    assert!(!scan.contains("dist/bundle.js"), "still ignored: {scan}");
}

/// The commit gate looks a verdict up by the hash of the whole staged diff, so a
/// review that skipped ignored files must still record it for the whole diff
/// (otherwise every commit that touches a lockfile is reviewed twice).
fn staged_gate(stage: &[(&str, &str)]) -> i32 {
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
    std::fs::write(p.join("package-lock.json"), "{}\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    for (path, content) in stage {
        std::fs::write(p.join(path), content).unwrap();
    }
    git(&["add", "-A"]);

    let home = tempfile::tempdir().unwrap();
    let review = cora(p, home.path())
        .args(["review", "--staged", "--no-llm"])
        .output()
        .unwrap();
    assert!(review.status.success(), "{review:?}");
    cora(p, home.path())
        .args(["hook", "verify", "--quiet"])
        .status()
        .unwrap()
        .code()
        .unwrap()
}

#[test]
fn a_verdict_is_recorded_for_the_whole_staged_diff_when_a_lockfile_is_part_of_it() {
    let code = staged_gate(&[
        ("src/lib.rs", "pub fn real() { let _x = 1; }\n"),
        ("package-lock.json", "{\"a\": 1}\n"),
    ]);
    assert_eq!(
        code, 0,
        "the commit gate must accept the reviewed staged diff"
    );
}

#[test]
fn a_staged_diff_of_ignored_files_only_passes_the_gate_too() {
    let code = staged_gate(&[("package-lock.json", "{\"a\": 1}\n")]);
    assert_eq!(
        code, 0,
        "nothing reviewable must not leave the commit unvalidated"
    );
}
