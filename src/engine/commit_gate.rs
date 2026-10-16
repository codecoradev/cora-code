//! Commit gate (#674): a verifiable record that a staged diff was reviewed.
//!
//! Three tiers share one record:
//!
//! 1. an agent judges the staged diff (`cora.review_finalize`) or a person runs
//!    `cora review --staged`; either records a [`Marker`] for that exact change
//!    set in `<git-dir>/cora/validations/`;
//! 2. the pre-commit hook asks [`check`] first and only falls back to a full
//!    review when there is no marker;
//! 3. an agent's `git commit` can be denied by `cora hook claude-guard`, which
//!    asks the same question.
//!
//! A marker is keyed by a fingerprint of the *change set* (file paths plus the
//! added and removed lines), not of the diff text, so it survives different
//! context sizes, `index` line abbreviations, and prefix settings. It carries no
//! source code: only the fingerprint, who judged, and the verdict.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engine::diff_parser::{DiffLineType, parse_diff};

/// `cora hook verify`: the staged diff is validated and not blocking.
pub const EXIT_VALIDATED: i32 = 0;
/// `cora hook verify`: validated, but the verdict blocks the commit.
pub const EXIT_BLOCKING: i32 = 10;
/// `cora hook verify`: no validation recorded for this staged diff.
pub const EXIT_NOT_VALIDATED: i32 = 11;

/// A marker older than this is ignored.
const MAX_AGE_SECS: u64 = 24 * 60 * 60;
/// At most this many markers are kept per repository.
const KEEP_MARKERS: usize = 50;

/// What was recorded about one reviewed change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// Fingerprint of the change set, see [`fingerprint`].
    pub fingerprint: String,
    /// Who judged: `agent`, `llm` or `deterministic`.
    pub judged_by: String,
    /// The verdict blocks the commit (a finding at or above the severity).
    pub should_block: bool,
    /// Findings in the verdict.
    pub total_issues: usize,
    /// Unix seconds.
    pub recorded_at: u64,
    /// cora version that wrote it.
    pub cora_version: String,
}

/// The answer to "may this staged diff be committed?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing reviewable is staged (empty, or only renames and mode changes).
    NothingToReview,
    /// Reviewed, and the verdict does not block.
    Validated(Marker),
    /// Reviewed, and the verdict blocks.
    Blocking(Marker),
    /// No usable record: never reviewed, edited since, or expired.
    NotValidated,
}

impl Status {
    /// Process exit code for `cora hook verify`.
    pub fn exit_code(&self) -> i32 {
        match self {
            Status::NothingToReview | Status::Validated(_) => EXIT_VALIDATED,
            Status::Blocking(_) => EXIT_BLOCKING,
            Status::NotValidated => EXIT_NOT_VALIDATED,
        }
    }
}

/// The reviewable core of a diff: for every file, its path and its added and
/// removed lines in order. Context lines, hunk headers, `index` lines and
/// prefixes are left out on purpose. Empty when nothing reviewable changed.
fn change_set(diff: &str) -> String {
    let mut out = String::new();
    for chunk in parse_diff(diff) {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        let mut body = String::new();
        if chunk.is_binary {
            body.push_str("binary\n");
        }
        for hunk in &chunk.chunks {
            let mut hunk_body = String::new();
            for line in &hunk.lines {
                match line.line_type {
                    DiffLineType::Add => {
                        hunk_body.push('+');
                        hunk_body.push_str(&line.content);
                        hunk_body.push('\n');
                    }
                    DiffLineType::Remove => {
                        hunk_body.push('-');
                        hunk_body.push_str(&line.content);
                        hunk_body.push('\n');
                    }
                    DiffLineType::Context => {}
                }
            }
            if !hunk_body.is_empty() {
                // The position matters: the same lines moved elsewhere are a different change.
                body.push_str(&format!("@@ -{} +{}\n", hunk.old_start, hunk.new_start));
                body.push_str(&hunk_body);
            }
        }
        if !body.is_empty() {
            out.push_str("file:");
            out.push_str(path);
            out.push('\n');
            out.push_str(&body);
        }
    }
    if out.is_empty() && !diff.trim().is_empty() {
        // Mode, symlink or rename-only changes carry no text lines: bind the verdict to the raw diff.
        return format!("raw:{diff}");
    }
    out
}

/// Fingerprint of `diff`'s change set; `None` when there is nothing to review.
pub fn fingerprint(diff: &str) -> Option<String> {
    let set = change_set(diff);
    if set.is_empty() {
        return None;
    }
    let digest = Sha256::digest(set.as_bytes());
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Absolute git directory of the current repository (worktree-aware).
pub fn git_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

fn validations_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("cora").join("validations")
}

/// Record `marker` (replacing a marker for the same change set) and prune old
/// ones.
pub fn record(git_dir: &Path, marker: &Marker) -> std::io::Result<()> {
    let dir = validations_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    // The fingerprint is hex, so it is a safe file name.
    if !marker.fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fingerprint must be hex",
        ));
    }
    let json = serde_json::to_string_pretty(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join(format!("{}.json", marker.fingerprint)), json)?;
    prune(&dir);
    Ok(())
}

/// Drop expired markers and keep only the newest [`KEEP_MARKERS`].
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = now_secs();
    let mut files: Vec<(u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let at = std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str::<Marker>(&s).ok())
                .map_or(0, |m| m.recorded_at);
            (at, p)
        })
        .collect();
    files.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (i, (at, p)) in files.iter().enumerate() {
        if i >= KEEP_MARKERS || now.saturating_sub(*at) > 7 * MAX_AGE_SECS {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Is `diff` (the staged change set) validated?
pub fn check(git_dir: &Path, diff: &str) -> Status {
    check_at(git_dir, diff, now_secs())
}

fn check_at(git_dir: &Path, diff: &str, now: u64) -> Status {
    let Some(fp) = fingerprint(diff) else {
        return Status::NothingToReview;
    };
    let path = validations_dir(git_dir).join(format!("{fp}.json"));
    let Some(marker) = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Marker>(&s).ok())
    else {
        return Status::NotValidated;
    };
    // The file name is the fingerprint, but never trust it alone.
    if marker.fingerprint != fp || now.saturating_sub(marker.recorded_at) > MAX_AGE_SECS {
        return Status::NotValidated;
    }
    if marker.should_block {
        Status::Blocking(marker)
    } else {
        Status::Validated(marker)
    }
}

/// Record a verdict for `diff` in the current repository. Best effort: no git
/// repository, nothing reviewable, or an unwritable directory all return
/// `false` and never fail the review.
pub fn record_for_diff(
    diff: &str,
    judged_by: &str,
    should_block: bool,
    total_issues: usize,
) -> bool {
    let (Some(fp), Some(dir)) = (fingerprint(diff), git_dir()) else {
        return false;
    };
    let marker = Marker {
        fingerprint: fp,
        judged_by: judged_by.to_string(),
        should_block,
        total_issues,
        recorded_at: now_secs(),
        cora_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    record(&dir, &marker).is_ok()
}

/// The staged diff of the current repository, or `None` outside a repository.
pub fn staged_diff() -> Option<String> {
    crate::git::get_staged_diff().ok()
}

/// Does a shell command line run `git commit`? And with which bypass?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CommitCall {
    /// The command contains a `git commit`.
    pub is_commit: bool,
    /// ... with `--no-verify` or a short-flag cluster containing `n`.
    pub no_verify: bool,
    /// ... that would commit more than the index (`-a`, `-i`, `-o`, `--`), or
    /// that changes the index in the same command line.
    pub beyond_index: bool,
}

/// Inspect a shell command line (as an agent's Bash tool would run it).
///
/// Commands are split on `;`, `&&`, `||`, `|` and newlines; each segment is
/// read as `[VAR=x ...] git [global options] commit [flags]`. Anything it
/// cannot read is "not a commit", so the guard never blocks what it does not
/// understand.
pub fn inspect_command(command: &str) -> CommitCall {
    let mut call = CommitCall::default();
    let normalized = command.replace([';', '|', '&'], "\n");
    let mut mutates_index = false;
    for segment in normalized.lines() {
        let mut tokens = segment.split_whitespace().peekable();
        // Leading environment assignments: `GIT_AUTHOR_NAME=x git commit`.
        while tokens
            .peek()
            .is_some_and(|t| t.contains('=') && !t.starts_with('-') && !t.contains('/'))
        {
            tokens.next();
        }
        let Some(first) = tokens.next() else {
            continue;
        };
        if first.rsplit('/').next() != Some("git") {
            continue;
        }
        // Skip global options until the subcommand.
        let mut sub: Option<&str> = None;
        while let Some(t) = tokens.next() {
            match t {
                "-c" => {
                    // `-c core.hooksPath=...` switches the hooks off.
                    if tokens
                        .next()
                        .is_some_and(|v| v.to_ascii_lowercase().contains("hookspath"))
                    {
                        call.no_verify = true;
                    }
                }
                "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                    tokens.next();
                }
                _ if t.starts_with("--git-dir=")
                    || t.starts_with("--work-tree=")
                    || t.starts_with("--namespace=") => {}
                "--no-pager"
                | "--paginate"
                | "-p"
                | "--bare"
                | "--no-replace-objects"
                | "--literal-pathspecs"
                | "--no-optional-locks" => {}
                other => {
                    sub = Some(other);
                    break;
                }
            }
        }
        if matches!(
            sub,
            Some("add" | "stage" | "rm" | "mv" | "apply" | "restore")
        ) {
            mutates_index = true;
        }
        if sub != Some("commit") {
            continue;
        }
        call.is_commit = true;
        for t in tokens {
            if t == "--"
                || t.starts_with("--all")
                || t.starts_with("--include")
                || t.starts_with("--only")
            {
                call.beyond_index = true;
            } else if t.len() >= 5 && "--no-verify".starts_with(t) && t.starts_with("--no-v") {
                // git accepts any unambiguous prefix of a long option.
                call.no_verify = true;
            } else if t.len() > 1
                && t.starts_with('-')
                && !t.starts_with("--")
                && t[1..].chars().all(|c| c.is_ascii_alphabetic())
                && t[1..].contains(['a', 'i', 'o'])
            {
                call.beyond_index = true;
            }
            if t.len() > 1
                && t.starts_with('-')
                && !t.starts_with("--")
                && t[1..].chars().all(|c| c.is_ascii_alphabetic())
                && t[1..].contains('n')
            {
                // `git commit -n` is `--no-verify`; `-nm`, `-an` too.
                call.no_verify = true;
            }
        }
    }
    if call.is_commit && mutates_index {
        call.beyond_index = true;
    }
    call
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_parsing_fails_closed_on_known_bypasses() {
        for c in [
            "git commit --no-verif -m x",
            "git commit --no-v -m x",
            "git -c core.hooksPath=/dev/null commit -m x",
            "true & git commit -n -m x",
        ] {
            assert!(inspect_command(c).no_verify, "{c}");
        }
        for c in [
            "git commit -am x",
            "git commit --all -m x",
            "git commit -m x -- file.rs",
            "git add . && git commit -m x",
            "git add . & git commit -m x",
        ] {
            assert!(inspect_command(c).beyond_index, "{c}");
        }
        assert!(!inspect_command("git commit -m x").beyond_index);
        assert!(!inspect_command("git commit -m x").no_verify);
    }

    #[test]
    fn the_fingerprint_binds_position_and_non_text_changes() {
        let moved = DIFF.replace("@@ -1,3 +1,4 @@", "@@ -10,3 +10,4 @@");
        assert_ne!(fingerprint(DIFF), fingerprint(&moved));
        let mode = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        assert!(fingerprint(mode).is_some());
    }

    const DIFF: &str = concat!(
        "diff --git a/app.py b/app.py\n",
        "index 1111111..2222222 100644\n",
        "--- a/app.py\n",
        "+++ b/app.py\n",
        "@@ -1,3 +1,4 @@\n",
        " import os\n",
        "+password = \"hunter2hunter2\"\n",
        " print(1)\n",
        " print(2)\n",
    );

    fn marker(fp: &str, block: bool, at: u64) -> Marker {
        Marker {
            fingerprint: fp.to_string(),
            judged_by: "agent".into(),
            should_block: block,
            total_issues: 1,
            recorded_at: at,
            cora_version: "test".into(),
        }
    }

    #[test]
    fn fingerprint_ignores_context_size_index_lines_and_hunk_headers() {
        let wide = concat!(
            "diff --git a/app.py b/app.py\n",
            "index aaaaaaa..bbbbbbb 100644\n",
            "--- a/app.py\n",
            "+++ b/app.py\n",
            "@@ -1,2 +1,3 @@ def f():\n",
            " import os\n",
            "+password = \"hunter2hunter2\"\n",
            " print(1)\n",
        );
        assert_eq!(fingerprint(DIFF), fingerprint(wide));
        assert!(fingerprint(DIFF).is_some());
    }

    #[test]
    fn fingerprint_changes_when_a_line_or_a_path_changes() {
        let edited = DIFF.replace("hunter2hunter2", "hunter3hunter3");
        assert_ne!(fingerprint(DIFF), fingerprint(&edited));
        let moved = DIFF.replace("app.py", "other.py");
        assert_ne!(fingerprint(DIFF), fingerprint(&moved));
        let removed = DIFF.replace("+password", "-password");
        assert_ne!(fingerprint(DIFF), fingerprint(&removed));
    }

    #[test]
    fn nothing_reviewable_has_no_fingerprint() {
        assert_eq!(fingerprint(""), None);
        let rename =
            "diff --git a/a.py b/b.py\nsimilarity index 100%\nrename from a.py\nrename to b.py\n";
        // Non-text changes are not "nothing": they need a verdict of their own.
        assert!(fingerprint(rename).is_some());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check(dir.path(), rename), Status::NotValidated);
    }

    #[test]
    fn record_then_check_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let fp = fingerprint(DIFF).unwrap();
        assert_eq!(check(dir.path(), DIFF), Status::NotValidated);
        record(dir.path(), &marker(&fp, false, now_secs())).unwrap();
        assert!(matches!(check(dir.path(), DIFF), Status::Validated(_)));
        assert_eq!(check(dir.path(), DIFF).exit_code(), EXIT_VALIDATED);
        // Edited since: a different change set has no marker.
        let edited = DIFF.replace("hunter2hunter2", "other-value-123");
        assert_eq!(check(dir.path(), &edited), Status::NotValidated);
        assert_eq!(check(dir.path(), &edited).exit_code(), EXIT_NOT_VALIDATED);
    }

    #[test]
    fn a_blocking_verdict_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let fp = fingerprint(DIFF).unwrap();
        record(dir.path(), &marker(&fp, true, now_secs())).unwrap();
        let status = check(dir.path(), DIFF);
        assert!(matches!(status, Status::Blocking(_)));
        assert_eq!(status.exit_code(), EXIT_BLOCKING);
    }

    #[test]
    fn expired_and_forged_markers_are_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let fp = fingerprint(DIFF).unwrap();
        let old = now_secs() - MAX_AGE_SECS - 10;
        record(dir.path(), &marker(&fp, false, old)).unwrap();
        assert_eq!(check(dir.path(), DIFF), Status::NotValidated, "expired");

        // A marker file whose content names another fingerprint is ignored.
        let other = "0".repeat(64);
        let forged = marker(&other, false, now_secs());
        let path = validations_dir(dir.path()).join(format!("{fp}.json"));
        std::fs::write(path, serde_json::to_string(&forged).unwrap()).unwrap();
        assert_eq!(check(dir.path(), DIFF), Status::NotValidated, "forged");

        // Garbage is not a marker either.
        let path = validations_dir(dir.path()).join(format!("{fp}.json"));
        std::fs::write(path, "not json").unwrap();
        assert_eq!(check(dir.path(), DIFF), Status::NotValidated, "garbage");
    }

    #[test]
    fn record_rejects_a_fingerprint_that_is_not_hex() {
        let dir = tempfile::tempdir().unwrap();
        let bad = marker("../../escape", false, now_secs());
        assert!(record(dir.path(), &bad).is_err());
        assert!(!dir.path().join("escape.json").exists());
    }

    #[test]
    fn pruning_keeps_only_the_newest_markers() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(KEEP_MARKERS + 10) {
            let fp = format!("{i:064x}");
            record(dir.path(), &marker(&fp, false, now_secs() - (i as u64))).unwrap();
        }
        let n = std::fs::read_dir(validations_dir(dir.path()))
            .unwrap()
            .count();
        assert!(n <= KEEP_MARKERS, "kept {n}");
    }

    // ─── inspect_command ───

    #[test]
    fn plain_and_decorated_commits_are_found() {
        for cmd in [
            "git commit -m 'x'",
            "git commit",
            "git -C /tmp/repo commit -s -m x",
            "git -c user.name=a commit -m x",
            "/usr/bin/git commit --amend",
            "GIT_AUTHOR_NAME=a git commit -m x",
            "git add -A && git commit -m x",
            "git add a; git commit -m x",
            "cd r && git --no-pager commit -m x",
            "git status\ngit commit -m x",
        ] {
            let c = inspect_command(cmd);
            assert!(c.is_commit, "{cmd}");
            assert!(!c.no_verify, "{cmd}");
        }
    }

    #[test]
    fn commands_that_are_not_commits_are_left_alone() {
        for cmd in [
            "git status",
            "git log --oneline",
            "git commit-tree abc",
            "echo git commit",
            "cargo test",
            "git diff --cached",
            "grep commit file.txt",
            "",
        ] {
            assert!(!inspect_command(cmd).is_commit, "{cmd}");
        }
    }

    #[test]
    fn bypass_flags_are_detected() {
        for cmd in [
            "git commit --no-verify -m x",
            "git commit -n -m x",
            "git commit -nm x",
            "git commit -an -m x",
            "git add . && git commit -m x --no-verify",
        ] {
            let c = inspect_command(cmd);
            assert!(c.is_commit && c.no_verify, "{cmd}");
        }
        // `-m` values and long flags that merely contain an n are not bypasses.
        for cmd in [
            "git commit -m 'no-verify'",
            "git commit --amend --no-edit",
            "git commit -am 'fix n'",
            "git commit -s -m x",
        ] {
            assert!(!inspect_command(cmd).no_verify, "{cmd}");
        }
    }
}
