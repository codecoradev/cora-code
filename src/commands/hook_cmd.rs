use anyhow::Result;
use colored::Colorize;

use crate::hook;

/// Execute `hook install` — install the pre-commit hook.
pub fn execute_hook_install() -> Result<()> {
    let path = hook::install_hook()?;

    println!(
        "{} Installed pre-commit hook to {}",
        "✅".green().bold(),
        path
    );
    println!(
        "{}",
        "   Before each commit the hook accepts a staged diff an agent already judged (`cora hook verify`),"
            .dimmed()
    );
    println!(
        "{}",
        "   otherwise it runs `cora review --staged --ci --severity major`.".dimmed()
    );
    println!("{}", "   Use `cora hook uninstall` to remove.".dimmed());

    Ok(())
}

/// Execute `hook uninstall` — remove the pre-commit hook.
pub fn execute_hook_uninstall() -> Result<()> {
    hook::uninstall_hook()?;

    println!("{} Pre-commit hook removed.", "✅".green().bold());

    Ok(())
}

/// Execute `hook verify`: may the staged diff be committed?
///
/// Exit codes: 0 validated (or nothing reviewable staged), 10 validated but the
/// verdict blocks, 11 not validated. Outside a git repository it is 0: there is
/// nothing to gate.
pub fn execute_hook_verify(quiet: bool) -> Result<i32> {
    use crate::engine::commit_gate::{self, Status};

    let (Some(git_dir), Some(diff)) = (commit_gate::git_dir(), commit_gate::staged_diff()) else {
        return Ok(commit_gate::EXIT_VALIDATED);
    };
    let status = commit_gate::check(&git_dir, &diff);
    if !quiet {
        match &status {
            Status::NothingToReview => println!("Nothing reviewable is staged."),
            Status::Validated(m) => println!(
                "Staged diff validated ({}, {} finding(s), not blocking).",
                m.judged_by, m.total_issues
            ),
            Status::Blocking(m) => println!(
                "Staged diff was reviewed ({}) and the verdict BLOCKS the commit ({} finding(s)). Fix the findings and review again.",
                m.judged_by, m.total_issues
            ),
            Status::NotValidated => println!(
                "Staged diff is not validated. Review it first: the cora-review skill / `cora.review_prepare` + `cora.review_finalize` (agent), or `cora review --staged --ci --severity major`."
            ),
        }
    }
    Ok(status.exit_code())
}

/// Execute `hook claude-guard`: a Claude Code `PreToolUse` hook for the Bash
/// tool. Reads the hook JSON on stdin and denies (exit 2, reason on stderr) a
/// `git commit` that bypasses the gate (`--no-verify`) or whose staged diff is
/// not validated. Everything else, and anything it cannot read, is allowed.
pub fn execute_claude_guard() -> Result<i32> {
    use crate::engine::commit_gate::{self, Status};
    use std::io::Read;

    let mut raw = String::new();
    std::io::stdin()
        .take(1_000_000)
        .read_to_string(&mut raw)
        .ok();
    let Ok(input) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(0);
    };
    if input.get("tool_name").and_then(|v| v.as_str()) != Some("Bash") {
        return Ok(0);
    }
    let Some(command) = input
        .pointer("/tool_input/command")
        .and_then(|v| v.as_str())
    else {
        return Ok(0);
    };
    let call = commit_gate::inspect_command(command);
    if !call.is_commit {
        return Ok(0);
    }
    if call.no_verify {
        eprintln!(
            "commit gate: `git commit --no-verify` (or -n) is not allowed. Fix the findings instead of bypassing the pre-commit gate."
        );
        return Ok(2);
    }
    if call.beyond_index {
        eprintln!(
            "commit gate: this commit may include changes that were not staged and reviewed (-a/-i/-o, a pathspec, or `git add` in the same command). Stage first, review the staged diff, then run a plain `git commit`."
        );
        return Ok(2);
    }
    let (Some(git_dir), Some(diff)) = (commit_gate::git_dir(), commit_gate::staged_diff()) else {
        return Ok(0); // not a repository, or git failed: nothing to gate
    };
    match commit_gate::check(&git_dir, &diff) {
        Status::NothingToReview | Status::Validated(_) => Ok(0),
        Status::Blocking(m) => {
            eprintln!(
                "commit gate: the staged diff was reviewed ({}) and the verdict blocks the commit ({} finding(s)). Fix them, `git add`, review again, then commit.",
                m.judged_by, m.total_issues
            );
            Ok(2)
        }
        Status::NotValidated => {
            eprintln!(
                "commit gate: the staged diff has not been reviewed. Run the cora-review skill on `git diff --cached` \
(cora.review_prepare, judge, cora.review_finalize), or `cora review --staged --ci --severity major`, then commit. \
Stage first: a `git add && git commit` chain is checked before the add runs."
            );
            Ok(2)
        }
    }
}
