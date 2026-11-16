//! Hermes Agent (Nous Research): a `pre_llm_call` shell hook that injects the
//! `cora hook context` block into the current user message, the MCP server and
//! the `cora-code` skill.
//!
//! `~/.hermes/config.yaml` is the user's file and may hold comments, so it is
//! edited as text, never re-serialized: one marked block is added for the keys
//! (`hooks`, `mcp_servers`) the file does not already define at top level. A
//! second top-level key of the same name would silently replace the user's
//! (YAML takes the last), so when one exists cora prints the snippet instead
//! of writing it.

use super::{
    CODE_SKILL, Ctx, Integration, Report, Step, backup_once, read_existing, remove_file_step,
    write_if_changed,
};
use std::path::PathBuf;

pub struct Hermes;

const BEGIN: &str = "# cora:begin (managed by `cora install`)";
const END: &str = "# cora:end";

fn root(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".hermes")
}
fn config(ctx: &Ctx) -> PathBuf {
    root(ctx).join("config.yaml")
}
fn skill(ctx: &Ctx) -> PathBuf {
    root(ctx)
        .join("skills")
        .join("code-intelligence")
        .join("cora-code")
        .join("SKILL.md")
}

/// Hermes starts MCP servers with a reduced environment, so name the binary by
/// its full path.
fn binary(ctx: &Ctx) -> String {
    if ctx.cora.contains('/') || ctx.cora.contains('\\') {
        return ctx.cora.clone();
    }
    which::which(&ctx.cora)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| ctx.cora.clone())
}

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn hooks_yaml(ctx: &Ctx) -> String {
    let abs = Ctx {
        cora: binary(ctx),
        ..ctx.clone()
    };
    format!(
        "hooks:\n  pre_llm_call:\n    - command: {}\n      timeout: 15\n",
        yaml_str(&format!("{} hook context --agent hermes", abs.cora_shell()))
    )
}

fn mcp_yaml(ctx: &Ctx) -> String {
    format!(
        "mcp_servers:\n  cora:\n    command: {}\n    args: [\"mcp\"]\n",
        yaml_str(&binary(ctx))
    )
}

/// Does the file define `key:` at top level (outside our own block)?
fn has_top_level_key(text: &str, key: &str) -> bool {
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with(BEGIN) {
            inside = true;
        } else if line.starts_with(END) {
            inside = false;
        } else if !inside
            && line
                .trim_end()
                .strip_prefix(key)
                .is_some_and(|r| r.starts_with(':'))
        {
            return true;
        }
    }
    false
}

/// `text` without our block.
fn strip_block(text: &str) -> String {
    let (Some(a), Some(b)) = (text.find(BEGIN), text.find(END)) else {
        return text.to_string();
    };
    if a > b {
        return text.to_string();
    }
    let end = text[b..].find('\n').map_or(text.len(), |i| b + i + 1);
    let head = text[..a].trim_end_matches('\n');
    let tail = &text[end..];
    match (head.is_empty(), tail.trim().is_empty()) {
        (true, _) => tail.to_string(),
        (false, true) => format!("{head}\n"),
        (false, false) => format!("{head}\n\n{tail}"),
    }
}

fn install_config(ctx: &Ctx, r: &mut Report) {
    let path = config(ctx);
    let existing = match read_existing(&path) {
        Ok(text) => text.unwrap_or_default(),
        Err(e) => {
            r.push("config", Step::Failed(e));
            return;
        }
    };
    let base = strip_block(&existing);

    let mut body = String::new();
    let mut manual = Vec::new();
    let wants = [("hooks", hooks_yaml(ctx)), ("mcp_servers", mcp_yaml(ctx))];
    for (key, yaml) in wants {
        if key == "mcp_servers" && !ctx.register_mcp {
            continue;
        }
        if has_top_level_key(&base, key) {
            manual.push((key, yaml));
        } else {
            body.push_str(&yaml);
        }
    }
    for (key, yaml) in &manual {
        r.todo.push(format!(
            "{} already has a top-level `{key}:`; add this under it yourself:\n{}",
            path.display(),
            yaml.lines()
                .map(|l| format!("      {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    let wanted = if body.is_empty() {
        base.clone()
    } else {
        let mut s = base.trim_end().to_string();
        if !s.is_empty() {
            s.push_str("\n\n");
        }
        s.push_str(&format!("{BEGIN}\n{body}{END}\n"));
        s
    };
    let step = if wanted == existing {
        Step::Unchanged(path.display().to_string())
    } else {
        if !existing.is_empty() {
            backup_once(&path, ctx);
        }
        write_if_changed(&path, &wanted, ctx)
    };
    r.push("config", step);
}

fn remove_config(ctx: &Ctx, r: &mut Report) {
    let path = config(ctx);
    let existing = match read_existing(&path) {
        Ok(Some(text)) => text,
        Ok(None) => {
            r.push(
                "config",
                Step::Unchanged(format!("{} not present", path.display())),
            );
            return;
        }
        Err(e) => {
            r.push("config", Step::Failed(e));
            return;
        }
    };
    let stripped = strip_block(&existing);
    if stripped == existing {
        r.push("config", Step::Unchanged("no cora block".into()));
    } else {
        r.push("config", write_if_changed(&path, &stripped, ctx));
    }
}

impl Integration for Hermes {
    fn name(&self) -> &'static str {
        "hermes"
    }

    fn detected(&self, ctx: &Ctx) -> bool {
        root(ctx).is_dir() || which::which("hermes").is_ok()
    }

    fn install(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("hermes", self.detected(ctx));
        install_config(ctx, &mut r);
        r.push("skill", write_if_changed(&skill(ctx), CODE_SKILL, ctx));
        r.todo.push(
            "Hermes asks you once to approve the new hook (start `hermes chat`, or set \
             HERMES_ACCEPT_HOOKS=1 / hooks_auto_accept: true)"
                .to_string(),
        );
        r
    }

    fn remove(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("hermes", self.detected(ctx));
        remove_config(ctx, &mut r);
        r.push("skill", remove_file_step(&skill(ctx), ctx));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(home: &std::path::Path) -> Ctx {
        Ctx {
            home: home.to_path_buf(),
            dry_run: false,
            force: false,
            no_instructions: false,
            register_mcp: true,
            cora: "/usr/bin/cora".into(),
        }
    }

    #[test]
    fn the_block_is_added_once_and_removal_restores_the_file_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let original = "# my hermes\nmodel: x\n\nterminal:\n  cwd: .\n";
        std::fs::create_dir_all(root(&c)).unwrap();
        std::fs::write(config(&c), original).unwrap();

        let r = Hermes.install(&c);
        assert!(!r.failed(), "{}", r.render());
        let text = std::fs::read_to_string(config(&c)).unwrap();
        assert!(text.starts_with(original.trim_end()), "{text}");
        assert!(text.contains("pre_llm_call") && text.contains("hook context --agent hermes"));
        assert!(text.contains("mcp_servers:"));
        // Valid YAML for the keys we added.
        assert_eq!(text.matches(BEGIN).count(), 1);

        let again = Hermes.install(&c);
        assert!(
            again
                .items
                .iter()
                .all(|i| matches!(i.step, Step::Unchanged(_))),
            "{}",
            again.render()
        );

        Hermes.remove(&c);
        assert_eq!(std::fs::read_to_string(config(&c)).unwrap(), original);
        assert!(!skill(&c).exists());
    }

    #[test]
    fn an_existing_top_level_key_is_never_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(root(&c)).unwrap();
        let original = "hooks:\n  on_session_start:\n    - command: mine\n";
        std::fs::write(config(&c), original).unwrap();

        let r = Hermes.install(&c);
        let text = std::fs::read_to_string(config(&c)).unwrap();
        assert_eq!(
            text.matches("\nhooks:").count() + usize::from(text.starts_with("hooks:")),
            1,
            "{text}"
        );
        assert!(
            text.contains("mcp_servers:"),
            "the missing key is still added"
        );
        assert!(
            r.todo
                .iter()
                .any(|t| t.contains("already has a top-level `hooks:`")),
            "{}",
            r.render()
        );
    }

    #[test]
    fn a_missing_config_file_is_created_and_removed_with_its_block() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        Hermes.install(&c);
        assert!(config(&c).exists());
        Hermes.remove(&c);
        assert!(
            std::fs::read_to_string(config(&c))
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn an_unreadable_config_is_reported_and_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(root(&c)).unwrap();
        std::fs::write(config(&c), [0xff, 0xfe]).unwrap(); // not UTF-8
        assert!(Hermes.install(&c).failed());
        assert!(Hermes.remove(&c).failed());
        assert_eq!(std::fs::read(config(&c)).unwrap(), vec![0xff, 0xfe]);
    }

    #[test]
    fn commented_out_keys_do_not_count_as_defined() {
        assert!(!has_top_level_key("# hooks:\n  hooks:\n", "hooks"));
        assert!(has_top_level_key("a: 1\nhooks:\n  x: y\n", "hooks"));
        assert!(!has_top_level_key(
            "a: 1\nhooks_auto_accept: true\n",
            "hooks"
        ));
    }
}
