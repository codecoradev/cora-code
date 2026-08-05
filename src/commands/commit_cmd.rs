//! `cora commit` subcommand — review staged diff + generate commit message + commit.

use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{self, Write};

use crate::config::schema::Config;
use crate::engine::llm;
use crate::engine::quality_gate;
use crate::engine::types::LLMConfig;
use crate::formatters::OutputFormat;

/// Exit codes.
const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_BLOCKED: i32 = 2;

/// Commit subcommand options.
pub struct CommitOptions {
    /// YOLO mode — auto-commit, no prompts.
    pub yolo: bool,
    /// Force commit even if quality gate fails.
    pub force: bool,
    /// Skip review, only generate commit message.
    pub no_review: bool,
    /// Always open editor to edit message.
    pub edit: bool,
    /// Stream LLM response.
    pub stream: bool,
    /// Quiet mode.
    pub quiet: bool,
}

/// Result of commit message generation.
#[derive(Debug, Clone)]
pub struct CommitMessage {
    /// The subject line (first line).
    pub subject: String,
    /// The body lines (after subject).
    pub body: String,
}

impl CommitMessage {
    /// Full message with subject + body.
    pub fn full(&self) -> String {
        if self.body.is_empty() {
            self.subject.clone()
        } else {
            format!("{}\n\n{}", self.subject, self.body)
        }
    }
}

/// Execute the `cora commit` subcommand.
pub async fn execute_commit(
    config: &Config,
    llm_config: &LLMConfig,
    opts: &CommitOptions,
) -> Result<i32> {
    // 1. Get staged diff
    let diff = crate::git::get_staged_diff()
        .map_err(|e| anyhow::anyhow!("Failed to get staged diff: {e}. Run `git add` first."))?;

    if diff.trim().is_empty() {
        if opts.quiet {
            return Ok(EXIT_OK);
        }
        eprintln!(
            "{}",
            "Nothing staged. Use `git add` to stage changes first.".yellow()
        );
        return Ok(EXIT_ERROR);
    }

    if !opts.quiet {
        let (files, lines) = diff_stats(&diff);
        eprintln!(
            "{}",
            format!("🔍 Reviewing staged changes ({files} files, {lines} lines)…").cyan()
        );
    }

    // 2. Review (unless --no-review)
    let _review_response = if opts.no_review {
        None
    } else {
        let response = crate::engine::review::review_diff_with_cache(
            config,
            llm_config,
            &diff,
            opts.stream,
            true,
            opts.quiet,
            None,
        )
        .await?;

        // Filter by severity
        let min_severity = config.hook.min_severity_level();
        let mut filtered = response.clone();
        filtered.issues.retain(|i| i.severity <= min_severity);

        // Quality gate
        let gate_result = if config.quality_gate.enabled {
            let result = quality_gate::evaluate(&filtered.issues, &config.quality_gate);
            Some(result)
        } else {
            None
        };

        // Check gate
        let gate_failed = gate_result
            .as_ref()
            .is_some_and(|g| g.status == quality_gate::GateStatus::Fail);

        // Save debt snapshot (best-effort)
        if config.debt.enabled {
            let (commit, branch) = get_git_context();
            let snapshot = crate::engine::debt_tracker::DebtSnapshot::from_review(
                &filtered.issues,
                gate_result.as_ref(),
                commit,
                branch,
                0,
                None,
                None,
            );
            crate::engine::debt_tracker::save_snapshot(
                &snapshot,
                config.debt.history_dir.as_deref(),
            );
        }

        if gate_failed && !opts.force {
            // Print findings
            let formatter = crate::formatters::formatter_for(OutputFormat::Pretty);
            let output = formatter.format_review(&filtered)?;
            print!("{output}");
            if let Some(gate) = &gate_result {
                print!("{}", quality_gate::format_gate_output(gate));
            }
            eprintln!(
                "\n{}",
                "Commit blocked. Fix issues or use: cora commit --force"
                    .red()
                    .bold()
            );
            return Ok(EXIT_BLOCKED);
        }

        // Print findings if any (even on pass)
        if !filtered.issues.is_empty() && !opts.quiet {
            let formatter = crate::formatters::formatter_for(OutputFormat::Compact);
            let output = formatter.format_review(&filtered)?;
            eprint!("{output}");
        }

        Some(filtered)
    };

    // 3. Generate commit message
    if !opts.quiet {
        eprintln!("{}", "  → Generating commit message…".dimmed());
    }

    let commit_msg = generate_commit_message(llm_config, &diff, opts.stream).await?;

    // 4. HITL or YOLO
    if opts.yolo || opts.edit {
        // YOLO: auto-accept
        // --edit: open editor
        let message = if opts.edit {
            open_editor(&commit_msg.full())?
        } else {
            commit_msg.full()
        };
        return do_commit(&message, opts.quiet);
    }

    // HITL: interactive prompt
    if opts.quiet {
        // Quiet mode: just commit with generated message
        return do_commit(&commit_msg.full(), opts.quiet);
    }

    // Show generated message
    eprintln!();
    eprintln!("{}", "📝 Generated commit message:".cyan().bold());
    eprintln!("{}", "─────────────────────────────────────────".dimmed());
    eprintln!("  {}", commit_msg.subject.white().bold());
    if !commit_msg.body.is_empty() {
        for line in commit_msg.body.lines() {
            eprintln!("  {}", line);
        }
    }
    eprintln!("{}", "─────────────────────────────────────────".dimmed());
    eprintln!();
    eprint!(
        "Accept commit message? [{}]es / [{}]dit / [{}]o › ",
        "Y".green(),
        "E".yellow(),
        "N".red()
    );
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();

    match answer.as_str() {
        "y" | "yes" | "" => do_commit(&commit_msg.full(), opts.quiet),
        "e" | "edit" => {
            let message = open_editor(&commit_msg.full())?;
            do_commit(&message, opts.quiet)
        }
        _ => {
            eprintln!("{}", "Commit aborted.".yellow());
            Ok(EXIT_ERROR)
        }
    }
}

/// Generate a conventional commit message from a diff.
async fn generate_commit_message(
    llm_config: &LLMConfig,
    diff: &str,
    stream: bool,
) -> Result<CommitMessage> {
    let user_prompt = build_commit_prompt(diff);

    let system_prompt = COMMIT_SYSTEM_PROMPT;

    let raw = if stream {
        llm::chat_completion_stream_raw(llm_config, system_prompt, &user_prompt).await?
    } else {
        llm::chat_completion_raw(llm_config, system_prompt, &user_prompt).await?
    };

    parse_commit_message(&raw)
}

/// Build the user prompt for commit message generation.
fn build_commit_prompt(diff: &str) -> String {
    // Truncate very long diffs for commit message generation.
    // Use floor_char_boundary to avoid panicking on multi-byte UTF-8
    // (e.g. emoji or non-ASCII characters in code/comments).
    let max_chars = 8000;
    let truncated = if diff.len() > max_chars {
        let mut end = max_chars;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        &diff[..end]
    } else {
        diff
    };

    format!(
        "Generate a conventional commit message for this diff.\n\n\
         Rules:\n\
         - Subject line: <type>(<scope>): <description>\n\
         - Types: feat, fix, refactor, perf, docs, test, chore, style, build, ci\n\
         - Scope: the module or area changed (e.g., auth, db, api)\n\
         - Description: lowercase, imperative mood, no period, max 72 chars\n\
         - Body: bullet points explaining WHAT changed and WHY, max 10 lines\n\
         - If multiple types, pick the most significant change\n\
         - Do NOT include Co-authored-by or other metadata\n\n\
         Diff:\n```diff\n{truncated}\n```"
    )
}

const COMMIT_SYSTEM_PROMPT: &str = r#"You are a commit message generator. You receive a git diff and produce a conventional commit message.

Output format — EXACTLY this JSON structure, nothing else:
{"subject":"type(scope): description","body":"- bullet point 1\n- bullet point 2"}

Rules:
- Subject: lowercase, imperative mood, no period, max 72 characters
- Type must be one of: feat, fix, refactor, perf, docs, test, chore, style, build, ci
- Scope: module or area changed (omit if unclear)
- Body: concise bullet points explaining changes, max 10 lines
- Be specific — mention actual file names, functions, or concepts changed
- If only one type of change, omit body bullets and keep subject only
- Always output valid JSON"#;

/// Extract JSON from LLM output (handles markdown fences, trailing text).
fn extract_json_from_raw(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();

    // Strip markdown code fences
    let content = if trimmed.starts_with("```") {
        let without_opening = trimmed.trim_start_matches('`');
        let without_lang = without_opening
            .find('\n')
            .map(|i| &without_opening[i + 1..])
            .unwrap_or(without_opening);
        without_lang.trim_end_matches('`').trim_end_matches('\n')
    } else {
        trimmed
    };

    // Find JSON object boundaries
    if let Some(start) = content.find('{') {
        let mut depth = 0;
        for (i, c) in content[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(&content[start..start + i + 1]);
                    }
                }
                _ => {}
            }
        }
    }

    anyhow::bail!("No JSON object found in LLM response")
}

/// Parse the LLM response into a CommitMessage.
fn parse_commit_message(raw: &str) -> Result<CommitMessage> {
    let json_str = extract_json_from_raw(raw)?;

    #[derive(serde::Deserialize)]
    struct RawCommitMsg {
        subject: String,
        #[serde(default)]
        body: String,
    }

    let parsed: RawCommitMsg =
        serde_json::from_str(json_str).with_context(|| "Invalid commit message JSON")?;

    // Clean up subject
    let subject = parsed.subject.trim().to_string();

    // Ensure subject starts with a conventional type
    let conventional_types = [
        "feat", "fix", "refactor", "perf", "docs", "test", "chore", "style", "build", "ci",
    ];
    let has_type = conventional_types
        .iter()
        .any(|t| subject.starts_with(t) || subject.starts_with(&format!("{t}(")));

    let subject = if has_type {
        subject
    } else {
        format!("chore: {subject}")
    };

    // Enforce max length on subject (char-boundary safe for UTF-8)
    let subject = if subject.len() > 72 {
        let mut end = 69;
        while !subject.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &subject[..end])
    } else {
        subject
    };

    Ok(CommitMessage {
        subject,
        body: parsed.body.trim().to_string(),
    })
}

/// Open $EDITOR to edit the commit message.
fn open_editor(initial: &str) -> Result<String> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    // Unique temp file using nanosecond timestamp
    let tmp_dir = std::env::temp_dir();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp_path = tmp_dir.join(format!("cora-commit-{nonce}.tmp"));

    std::fs::write(&tmp_path, initial)
        .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;

    // Split $EDITOR into program + args to avoid shell injection via `sh -c`.
    // Handles editors with flags like `code --wait` or `vim -p`.
    let editor_parts: Vec<&str> = editor.split_whitespace().collect();
    let (program, args) = match editor_parts.split_first() {
        Some((p, a)) => (p, a),
        None => anyhow::bail!("$EDITOR is empty"),
    };
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&tmp_path)
        .status()
        .with_context(|| format!("Failed to open editor: {editor}"))?;

    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!("Editor exited with non-zero status");
    }

    let content =
        std::fs::read_to_string(&tmp_path).with_context(|| "Failed to read edited message")?;

    let _ = std::fs::remove_file(&tmp_path);

    // Strip comment lines (lines starting with #)
    let cleaned: String = content
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<&str>>()
        .join("\n")
        .trim()
        .to_string();

    if cleaned.is_empty() {
        anyhow::bail!("Empty commit message");
    }

    Ok(cleaned)
}

/// Execute git commit with the given message.
fn do_commit(message: &str, quiet: bool) -> Result<i32> {
    // Unique temp file for commit message
    let tmp_dir = std::env::temp_dir();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let msg_path = tmp_dir.join(format!("cora-commit-{nonce}.msg"));
    std::fs::write(&msg_path, message).context("Failed to write commit message file")?;

    let status = std::process::Command::new("git")
        .args(["commit", "-F", &msg_path.to_string_lossy()])
        .status()
        .context("Failed to run git commit")?;

    let _ = std::fs::remove_file(&msg_path);

    if status.success() {
        if !quiet {
            let subject = message.lines().next().unwrap_or("committed");
            eprintln!("{}", format!("✅ Committed: {subject}").green().bold());
        }
        Ok(EXIT_OK)
    } else {
        eprintln!("{}", "❌ git commit failed".red().bold());
        Ok(EXIT_ERROR)
    }
}

/// Get git context (cached).
fn get_git_context() -> (Option<String>, Option<String>) {
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<(Option<String>, Option<String>)> = LazyLock::new(|| {
        let commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout)
                        .ok()
                        .map(|s| s.trim().to_string())
                } else {
                    None
                }
            });

        let branch = crate::git::get_current_branch().ok();

        (commit, branch)
    });

    (CONTEXT.0.clone(), CONTEXT.1.clone())
}

/// Count files and lines in a diff.
fn diff_stats(diff: &str) -> (usize, usize) {
    let mut files = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            files += 1;
        } else if line.starts_with('+') && !line.starts_with("+++") {
            added += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            removed += 1;
        }
    }

    (files.max(1), added + removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── parse_commit_message ───

    #[test]
    fn parse_valid_json() {
        let raw = r#"{"subject":"feat(auth): add session expiry","body":"- Add TTL config\n- Validate on request"}"#;
        let msg = parse_commit_message(raw).unwrap();
        assert_eq!(msg.subject, "feat(auth): add session expiry");
        assert!(msg.body.contains("TTL config"));
    }

    #[test]
    fn parse_wrapped_in_fences() {
        let raw =
            "```json\n{\"subject\":\"fix: null pointer\",\"body\":\"- Added null check\"}\n```";
        let msg = parse_commit_message(raw).unwrap();
        assert_eq!(msg.subject, "fix: null pointer");
    }

    #[test]
    fn parse_no_body() {
        let raw = r#"{"subject":"chore: update deps","body":""}"#;
        let msg = parse_commit_message(raw).unwrap();
        assert_eq!(msg.subject, "chore: update deps");
        assert!(msg.body.is_empty());
    }

    #[test]
    fn parse_adds_type_if_missing() {
        let raw = r#"{"subject":"update the readme","body":""}"#;
        let msg = parse_commit_message(raw).unwrap();
        assert!(msg.subject.starts_with("chore:"));
    }

    #[test]
    fn parse_truncates_long_subject() {
        let long = "a".repeat(80);
        let raw = format!(r#"{{"subject":"{long}","body":""}}"#);
        let msg = parse_commit_message(&raw).unwrap();
        assert!(msg.subject.len() <= 75); // 72 + "…"
    }

    #[test]
    fn parse_truncates_long_subject_with_multibyte() {
        // Subject with emoji near the truncation boundary (byte 69).
        // Without char-boundary checking, &subject[..69] would split the
        // 4-byte emoji at positions 67–70 and panic.
        let mut long = "x".repeat(67);
        long.push_str("🎉rest_of_subject_here"); // emoji at bytes 67-70
        let raw = format!(r#"{{"subject":"{long}","body":""}}"#);
        // Must not panic:
        let msg = parse_commit_message(&raw).unwrap();
        assert!(msg.subject.contains('…'));
    }

    #[test]
    fn parse_invalid_json_fails() {
        let result = parse_commit_message("not json at all");
        assert!(result.is_err());
    }

    // ─── build_commit_prompt ───

    #[test]
    fn commit_prompt_contains_diff() {
        let diff = "diff --git a/foo.rs b/foo.rs\n+new line";
        let prompt = build_commit_prompt(diff);
        assert!(prompt.contains("foo.rs"));
        assert!(prompt.contains("conventional commit"));
    }

    #[test]
    fn commit_prompt_truncates_multibyte_utf8_without_panic() {
        // Fill 7998 ASCII bytes, then a 4-byte emoji, then more text.
        // Without char-boundary checking, slicing at 8000 would land
        // mid-codepoint and panic with "byte index is not a char boundary".
        let mut diff = "a".repeat(7998);
        diff.push('🎉'); // 4 bytes: positions 7998..8002
        diff.push_str(&"b".repeat(200));

        // This must not panic:
        let prompt = build_commit_prompt(&diff);
        assert!(prompt.contains("conventional commit"));
        // Truncated output should not contain the partial emoji bytes
        // (it ends before the emoji because the boundary floors to 7998).
        assert!(!prompt.contains('🎉'));
    }

    // ─── diff_stats ───

    #[test]
    fn diff_stats_counts_files_and_lines() {
        let diff =
            "diff --git a/a.rs b/a.rs\n+line1\n+line2\ndiff --git a/b.rs b/b.rs\n+line3\n-old";
        let (files, lines) = diff_stats(diff);
        assert_eq!(files, 2);
        assert_eq!(lines, 4); // 3 added + 1 removed
    }

    #[test]
    fn diff_stats_empty() {
        let (files, lines) = diff_stats("");
        assert_eq!(files, 1); // max(1, 0)
        assert_eq!(lines, 0);
    }

    // ─── CommitMessage ───

    #[test]
    fn commit_message_full_no_body() {
        let msg = CommitMessage {
            subject: "feat: add x".to_string(),
            body: String::new(),
        };
        assert_eq!(msg.full(), "feat: add x");
    }

    #[test]
    fn commit_message_full_with_body() {
        let msg = CommitMessage {
            subject: "feat: add x".to_string(),
            body: "- did thing\n- did other".to_string(),
        };
        assert!(msg.full().contains("\n\n"));
    }
}
