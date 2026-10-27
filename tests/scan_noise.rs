//! `cora scan` noise controls (#683): clone hints are opt-in, `--min-severity`
//! trims every output, and the summary counts findings by severity.
use std::process::Command;

fn scan(format: &str, args: &[&str]) -> String {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(
        d.path().join("src/a.rs"),
        "fn f(a: &Arc<S>) { let b = a.clone(); }\nfn g() { let x = y.unwrap(); }\nlet password = \"hunter2hunter2\";\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cora"))
        .args(["scan", "--no-llm", "--format", format])
        .args(args)
        .current_dir(d.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", d.path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr)
}

#[test]
fn a_default_scan_does_not_list_clone_sites() {
    let text = scan("compact", &[]);
    assert!(!text.contains("qual-clone"), "{text}");
    assert!(text.contains("hardcoded-secret"), "{text}");
}

#[test]
fn the_summary_counts_findings_by_severity() {
    let text = scan("json", &[]);
    assert!(text.contains("1 critical"), "{text}");
    assert!(text.contains("minor"), "unwrap is minor: {text}");
}

#[test]
fn min_severity_hides_lower_findings_everywhere() {
    let text = scan("compact", &["--min-severity", "major"]);
    assert!(text.contains("hardcoded-secret"), "{text}");
    assert!(
        !text.contains("bug-unwrap"),
        "minor finding must be hidden: {text}"
    );
    assert!(!text.contains("minor"), "{text}");
}
