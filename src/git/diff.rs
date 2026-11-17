use std::process::Command;

use crate::error::CoraError;
use git2::Repository;

/// Characters that should not appear in git refs passed to our diff commands.
/// This prevents shell injection and path traversal via crafted ref names.
const DANGEROUS_REF_CHARS: &[char] = &[
    ';', '&', '|', '`', '$', '(', ')', '<', '>', '{', '}', '\\', '\n', '\r', '\t',
];

/// Validate a git ref string for safety before passing it to git commands.
///
/// Rejects refs containing shell metacharacters or path traversal sequences (..).
fn validate_ref(ref_str: &str) -> std::result::Result<(), CoraError> {
    if ref_str.contains("..") {
        // ".." is only allowed in the middle of range expressions like "HEAD..HEAD" or "main...HEAD"
        // But we need to be careful about path traversal. Allow it only for git range syntax.
        // Actually, git diff ranges use ".." and "..." which are valid git syntax.
        // The risk is when ".." appears as part of a path (e.g., "../../etc/passwd").
        // Since git refs shouldn't contain path components with "..", we reject refs
        // that start with ".." or contain "/.." or ".." at the end.
        if ref_str.starts_with("..") || ref_str.contains("/..") || ref_str.ends_with("..") {
            return Err(CoraError::InvalidRef(format!(
                "invalid ref '{ref_str}': contains path traversal sequence '..'"
            )));
        }
    }

    if ref_str.contains(DANGEROUS_REF_CHARS) {
        let found: String = ref_str
            .chars()
            .filter(|c| DANGEROUS_REF_CHARS.contains(c))
            .collect();
        return Err(CoraError::InvalidRef(format!(
            "invalid ref '{ref_str}': contains unsafe characters: '{found}'"
        )));
    }

    Ok(())
}

/// Open the git repository at or above the current working directory.
pub fn open_repo() -> std::result::Result<Repository, CoraError> {
    Repository::discover(std::env::current_dir()?).map_err(|_| CoraError::NotInGitRepo)
}

/// Run a git command and return its stdout as a string.
fn git_cmd(args: &[&str]) -> std::result::Result<String, CoraError> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| CoraError::GitCommand {
            command: args.join(" "),
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_str = stderr.trim();

        // Detect common git errors and provide friendly messages
        if stderr_str.contains("not a git repository")
            || stderr_str.contains("unknown option `cached'")
            || stderr_str.contains("Not a git repository")
        {
            return Err(CoraError::NotInGitRepo);
        }

        return Err(CoraError::GitCommand {
            command: args.join(" "),
            stderr: stderr.to_string(),
        });
    }

    String::from_utf8(output.stdout).map_err(|e| CoraError::GitCommand {
        command: args.join(" "),
        stderr: e.to_string(),
    })
}

/// Get the diff of currently staged changes (git diff --cached).
pub fn get_staged_diff() -> std::result::Result<String, CoraError> {
    git_cmd(&["diff", "--cached"])
}

/// Get the diff of unstaged changes (working tree vs index).
pub fn get_unstaged_diff() -> std::result::Result<String, CoraError> {
    git_cmd(&["diff"])
}

/// Get the diff between the current branch and `base_branch`.
pub fn get_branch_diff(base_branch: &str) -> std::result::Result<String, CoraError> {
    validate_ref(base_branch)?;
    let arg = format!("{base_branch}...HEAD");
    git_cmd(&["diff", &arg])
}

/// Get the diff of unpushed commits (HEAD vs @{u}).
pub fn get_unpushed_diff() -> std::result::Result<String, CoraError> {
    git_cmd(&["log", "-p", "@{u}..HEAD"])
}

/// Get the diff for a commit reference.
///
/// - If the ref contains `..` (e.g. `HEAD~3..HEAD`), uses `git diff <ref>`.
/// - Otherwise (e.g. `HEAD`, `abc123`), uses `git show <ref> --format=""` to
///   get just the diff without commit metadata.
pub fn get_commit_diff(ref_str: &str) -> std::result::Result<String, CoraError> {
    validate_ref(ref_str)?;
    if ref_str.contains("..") {
        git_cmd(&["diff", ref_str])
    } else {
        git_cmd(&["show", ref_str, "--format="])
    }
}

/// Get the current branch name.
pub fn get_current_branch() -> std::result::Result<String, CoraError> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| CoraError::GitCommand {
            command: "git rev-parse --abbrev-ref HEAD".into(),
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoraError::GitCommand {
            command: "git rev-parse --abbrev-ref HEAD".into(),
            stderr: stderr.to_string(),
        });
    }

    let name = String::from_utf8(output.stdout).map_err(|e| CoraError::GitCommand {
        command: "git rev-parse".into(),
        stderr: e.to_string(),
    })?;
    let name = name.trim().to_string();

    if name.is_empty() || name == "HEAD" {
        return Err(CoraError::HeadDetached);
    }

    Ok(name)
}

/// Try to get repository info: (owner, `repo_name`, branch).
/// Owner is derived from the remote URL if possible.
pub fn get_repo_info() -> std::result::Result<(String, String, String), CoraError> {
    let repo = open_repo()?;
    let branch = get_current_branch()?;

    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(std::string::ToString::to_string))
        .unwrap_or_default();

    let (owner, repo_name) = parse_remote_url(&remote_url);
    Ok((owner, repo_name, branch))
}

/// Parse a remote URL into (owner, `repo_name`).
fn parse_remote_url(url: &str) -> (String, String) {
    // Handle git@host:owner/repo.git, https://host/owner/repo.git, etc.
    let clean = url
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .trim_start_matches("git@");

    let parts: Vec<&str> = clean.split('/').collect();
    if parts.len() >= 2 {
        // git@host:owner/repo  => parts = ["host:owner", "repo"]
        let maybe_owner = parts[parts.len() - 2];
        let (owner, _) = maybe_owner.split_once(':').unwrap_or((maybe_owner, ""));
        let repo_name = parts[parts.len() - 1];
        (owner.to_string(), repo_name.to_string())
    } else {
        ("unknown".to_string(), "unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_ref_simple_name() {
        assert!(validate_ref("main").is_ok());
        assert!(validate_ref("HEAD").is_ok());
        assert!(validate_ref("feature/my-branch").is_ok());
        assert!(validate_ref("abc123").is_ok());
    }

    #[test]
    fn validate_ref_range_syntax() {
        assert!(validate_ref("HEAD~3..HEAD").is_ok());
        assert!(validate_ref("main...develop").is_ok());
        assert!(validate_ref("abc123..def456").is_ok());
    }

    #[test]
    fn validate_ref_rejects_path_traversal() {
        assert!(validate_ref("../../etc/passwd").is_err());
        assert!(validate_ref("..HEAD").is_err());
        assert!(validate_ref("HEAD..").is_err());
        assert!(validate_ref("branch/../other").is_err());
    }

    #[test]
    fn validate_ref_rejects_shell_metacharacters() {
        assert!(validate_ref("main;rm -rf /").is_err());
        assert!(validate_ref("main`whoami`").is_err());
        assert!(validate_ref("$(whoami)").is_err());
        assert!(validate_ref("main && echo pwned").is_err());
        assert!(validate_ref("main | cat").is_err());
        assert!(validate_ref("main\nextra").is_err());
    }

    #[test]
    fn validate_ref_rejects_angle_brackets() {
        assert!(validate_ref("main > /etc/passwd").is_err());
    }
}
