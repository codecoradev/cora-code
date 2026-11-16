//! Native integrations: install cora into the AI coding agents it knows well
//! (Claude Code, Hermes, omp, pi, opencode) so they use code search on every
//! prompt, not only when the model remembers a tool exists (#753).
//!
//! Each agent is an [`Integration`]: it can detect the agent, install what the
//! agent needs (MCP registration, a prompt hook, a skill, an instruction
//! block), remove it again, and report what it did. Everything is idempotent,
//! honors `--dry-run`, and never rewrites a file that would not change.

pub mod claude;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The `cora-code` skill, shared by every agent.
pub const CODE_SKILL: &str = include_str!("cora_code_skill.md");

/// What an installer needs to know about this run.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// The user's home directory.
    pub home: PathBuf,
    /// Report what would change, change nothing.
    pub dry_run: bool,
    /// Replace entries that exist but differ.
    pub force: bool,
    /// Do not touch instruction files (`CLAUDE.md`, `AGENTS.md`).
    pub no_instructions: bool,
    /// Register the MCP server by running the agent's own CLI.
    pub register_mcp: bool,
    /// How hooks should call cora: `cora` when it is on the PATH, else the
    /// absolute path of this binary.
    pub cora: String,
}

impl Ctx {
    /// A context for the current user.
    pub fn for_user(dry_run: bool, force: bool, no_instructions: bool) -> Result<Self> {
        let home = dirs::home_dir().context("cannot find the home directory")?;
        Ok(Self {
            home,
            dry_run,
            force,
            no_instructions,
            register_mcp: true,
            cora: cora_command(),
        })
    }
}

/// `cora` if the PATH finds one, else the absolute path of the running binary
/// (an agent's environment may not have the user's PATH).
pub fn cora_command() -> String {
    if which::which("cora").is_ok() {
        return "cora".to_string();
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "cora".to_string())
}

/// What happened to one thing an installer manages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Written (or removed).
    Done(String),
    /// Already as wanted.
    Unchanged(String),
    /// Not done, with the reason (not an error).
    Skipped(String),
    /// Failed, with the reason.
    Failed(String),
}

/// One managed thing of an agent: its MCP entry, a hook, a skill, an instruction block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub what: &'static str,
    pub step: Step,
}

/// The result of installing or removing for one agent.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub agent: &'static str,
    pub detected: bool,
    pub items: Vec<Item>,
    /// What the user still has to do by hand.
    pub todo: Vec<String>,
}

impl Report {
    pub fn new(agent: &'static str, detected: bool) -> Self {
        Self {
            agent,
            detected,
            ..Self::default()
        }
    }

    pub fn push(&mut self, what: &'static str, step: Step) {
        self.items.push(Item { what, step });
    }

    pub fn failed(&self) -> bool {
        self.items.iter().any(|i| matches!(i.step, Step::Failed(_)))
    }

    /// Plain-text rendering, one line per item.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{}{}\n",
            self.agent,
            if self.detected { "" } else { " (not detected)" }
        );
        for i in &self.items {
            let (mark, text) = match &i.step {
                Step::Done(t) => ("✓", t),
                Step::Unchanged(t) => ("=", t),
                Step::Skipped(t) => ("-", t),
                Step::Failed(t) => ("✗", t),
            };
            out.push_str(&format!("  {mark} {}: {text}\n", i.what));
        }
        for t in &self.todo {
            out.push_str(&format!("  → {t}\n"));
        }
        out
    }
}

/// An agent cora installs itself into.
pub trait Integration {
    /// Name used by `--agents`.
    fn name(&self) -> &'static str;
    /// Is the agent on this machine?
    fn detected(&self, ctx: &Ctx) -> bool;
    /// Install what the agent needs.
    fn install(&self, ctx: &Ctx) -> Report;
    /// Remove what [`Self::install`] added.
    fn remove(&self, ctx: &Ctx) -> Report;
}

/// Every native integration.
pub fn all() -> Vec<Box<dyn Integration>> {
    vec![Box::new(claude::Claude)]
}

/// Install (or remove) the native integrations for the detected agents, or for
/// the ones `agents` (comma-separated) names. Returns the report text.
pub fn run(
    agents: Option<&str>,
    dry_run: bool,
    force: bool,
    no_instructions: bool,
    remove: bool,
) -> Result<String> {
    let ctx = Ctx::for_user(dry_run, force, no_instructions)?;
    let wanted: Option<Vec<&str>> = agents.map(|l| l.split(',').map(str::trim).collect());
    let mut out = String::new();
    let mut failed = false;
    for i in all() {
        let picked = match &wanted {
            Some(w) => w.contains(&i.name()),
            None => i.detected(&ctx),
        };
        if !picked {
            continue;
        }
        let report = if remove {
            i.remove(&ctx)
        } else {
            i.install(&ctx)
        };
        out.push_str(&report.render());
        failed |= report.failed();
    }
    if failed {
        anyhow::bail!("{}\nsome integration steps failed", out.trim_end());
    }
    Ok(out.trim_end().to_string())
}

// ── helpers shared by the integrations ──────────────────────────────────────

/// Write `content` to `path` if it differs: create the directories, write to a
/// temporary file and rename. In a dry run nothing is written.
pub fn write_if_changed(path: &Path, content: &str, ctx: &Ctx) -> Step {
    if std::fs::read_to_string(path).is_ok_and(|c| c == content) {
        return Step::Unchanged(path.display().to_string());
    }
    if ctx.dry_run {
        return Step::Done(format!("would write {}", path.display()));
    }
    match write_atomic(path, content) {
        Ok(()) => Step::Done(format!("wrote {}", path.display())),
        Err(e) => Step::Failed(format!("{}: {e:#}", path.display())),
    }
}

/// Atomic write (temporary file in the same directory, then rename).
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let tmp = path.with_extension(format!("cora-tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Copy `path` to `<path>.cora-backup` once, before cora first changes it.
pub fn backup_once(path: &Path, ctx: &Ctx) {
    if ctx.dry_run || !path.exists() {
        return;
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(".cora-backup");
    let backup = PathBuf::from(name);
    if !backup.exists() {
        let _ = std::fs::copy(path, backup);
    }
}

/// Remove a file (or an empty skill directory around it) if it exists.
pub fn remove_file_step(path: &Path, ctx: &Ctx) -> Step {
    if !path.exists() {
        return Step::Unchanged(format!("{} not present", path.display()));
    }
    if ctx.dry_run {
        return Step::Done(format!("would remove {}", path.display()));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir); // only if it is now empty
            }
            Step::Done(format!("removed {}", path.display()))
        }
        Err(e) => Step::Failed(format!("{}: {e}", path.display())),
    }
}

/// Markers of a block cora manages inside a Markdown instruction file.
pub const BLOCK_BEGIN: &str = "<!-- cora:begin -->";
pub const BLOCK_END: &str = "<!-- cora:end -->";

/// Insert or replace the managed block in `existing`.
pub fn block_upsert(existing: &str, body: &str) -> String {
    let block = format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}", body.trim_end());
    match (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
        (Some(a), Some(b)) if a < b => {
            let end = b + BLOCK_END.len();
            format!("{}{}{}", &existing[..a], block, &existing[end..])
        }
        _ => {
            let mut s = existing.trim_end().to_string();
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(&block);
            s.push('\n');
            s
        }
    }
}

/// Remove the managed block; the file is left as it was without it.
pub fn block_remove(existing: &str) -> String {
    match (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
        (Some(a), Some(b)) if a < b => {
            let end = b + BLOCK_END.len();
            let head = existing[..a].trim_end();
            let tail = existing[end..].trim_start_matches('\n');
            if head.is_empty() {
                tail.to_string()
            } else if tail.is_empty() {
                format!("{head}\n")
            } else {
                format!("{head}\n\n{tail}")
            }
        }
        _ => existing.to_string(),
    }
}

/// Upsert the managed block into the instruction file at `path`.
pub fn upsert_instruction_block(path: &Path, body: &str, ctx: &Ctx) -> Step {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let wanted = block_upsert(&existing, body);
    if wanted == existing {
        return Step::Unchanged(path.display().to_string());
    }
    if !existing.is_empty() {
        backup_once(path, ctx);
    }
    write_if_changed(path, &wanted, ctx)
}

/// Remove the managed block from the instruction file at `path`.
pub fn remove_instruction_block(path: &Path, ctx: &Ctx) -> Step {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return Step::Unchanged(format!("{} not present", path.display()));
    };
    let wanted = block_remove(&existing);
    if wanted == existing {
        return Step::Unchanged(format!("no cora block in {}", path.display()));
    }
    if wanted.trim().is_empty() {
        if ctx.dry_run {
            return Step::Done(format!("would remove {}", path.display()));
        }
        return match std::fs::remove_file(path) {
            Ok(()) => Step::Done(format!(
                "removed {} (it only held the cora block)",
                path.display()
            )),
            Err(e) => Step::Failed(format!("{}: {e}", path.display())),
        };
    }
    write_if_changed(path, &wanted, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn ctx(home: &Path) -> Ctx {
        Ctx {
            home: home.to_path_buf(),
            dry_run: false,
            force: false,
            no_instructions: false,
            register_mcp: false,
            cora: "cora".to_string(),
        }
    }

    #[test]
    fn a_block_is_added_replaced_and_removed_without_touching_the_rest() {
        let doc = "# My rules\n\nBe kind.\n";
        let with = block_upsert(doc, "line one");
        assert!(with.starts_with(
            "# My rules\n\nBe kind.\n\n<!-- cora:begin -->\nline one\n<!-- cora:end -->"
        ));
        // idempotent
        assert_eq!(block_upsert(&with, "line one"), with);
        // replaced in place
        let changed = block_upsert(&with, "line two");
        assert!(changed.contains("line two") && !changed.contains("line one"));
        assert_eq!(changed.matches(BLOCK_BEGIN).count(), 1);
        // removal gives the original back
        assert_eq!(block_remove(&with), doc);
        // a file of only the block becomes empty
        assert!(block_remove(&block_upsert("", "x")).trim().is_empty());
        // no block: untouched
        assert_eq!(block_remove(doc), doc);
    }

    #[test]
    fn write_if_changed_writes_once_and_a_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a").join("b.txt");
        let mut c = ctx(tmp.path());
        c.dry_run = true;
        assert!(
            matches!(write_if_changed(&path, "x", &c), Step::Done(m) if m.starts_with("would"))
        );
        assert!(!path.exists());
        c.dry_run = false;
        assert!(matches!(write_if_changed(&path, "x", &c), Step::Done(_)));
        assert!(matches!(
            write_if_changed(&path, "x", &c),
            Step::Unchanged(_)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    #[test]
    fn the_first_change_to_an_instruction_file_leaves_a_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("CLAUDE.md");
        std::fs::write(&path, "mine\n").unwrap();
        let c = ctx(tmp.path());
        assert!(matches!(
            upsert_instruction_block(&path, "body", &c),
            Step::Done(_)
        ));
        let backup = tmp.path().join("CLAUDE.md.cora-backup");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "mine\n");
        assert!(matches!(remove_instruction_block(&path, &c), Step::Done(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    }

    #[test]
    fn the_code_skill_has_the_frontmatter_agents_require() {
        assert!(CODE_SKILL.starts_with("---\nname: cora-code\ndescription: "));
        assert!(CODE_SKILL.contains("cora_brain_search"));
    }
}
