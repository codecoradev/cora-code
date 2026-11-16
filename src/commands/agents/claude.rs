//! Claude Code: prompt/session hooks in `~/.claude/settings.json`, the MCP
//! server (registered through `claude mcp add-json`, always loaded), the
//! `cora-code` skill and a short block in `~/.claude/CLAUDE.md`.

use super::{
    CODE_SKILL, Ctx, Integration, Report, Step, backup_once, remove_file_step,
    remove_instruction_block, upsert_instruction_block, write_if_changed,
};
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct Claude;

const PROMPT_EVENT: &str = "UserPromptSubmit";
const SESSION_EVENT: &str = "SessionStart";
const PROMPT_MARK: &str = "hook context --agent claude";
const SESSION_MARK: &str = "hook session --agent claude";

const INSTRUCTIONS: &str = "## Code search (cora)\n\n\
Use cora to find code by meaning and to check impact before changing it: \
`cora_brain_search` for a question in words, `cora_find_callers` and \
`cora_find_impact` before editing a function, `cora_find_affected_tests` for \
the tests to run. Prefer them to guessing file names; read the files they \
point to. See the `cora-code` skill.";

fn settings_path(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".claude").join("settings.json")
}

fn hook_entry(command: &str, timeout: u64) -> Value {
    json!({ "hooks": [ { "type": "command", "command": command, "timeout": timeout } ] })
}

fn is_ours(entry: &Value, mark: &str) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter()
            .any(|h| h["command"].as_str().is_some_and(|c| c.contains(mark)))
    })
}

/// Add (or, with `force`, replace) our entry for `event`. Returns whether the
/// document changed.
fn set_hook(
    doc: &mut Value,
    event: &str,
    mark: &str,
    command: &str,
    timeout: u64,
    force: bool,
) -> bool {
    let list = &mut doc["hooks"][event];
    if !list.is_array() {
        *list = json!([]);
    }
    let arr = list.as_array_mut().expect("array");
    let wanted = hook_entry(command, timeout);
    if let Some(pos) = arr.iter().position(|e| is_ours(e, mark)) {
        if arr[pos] == wanted || !force {
            return false;
        }
        arr[pos] = wanted;
        return true;
    }
    arr.push(wanted);
    true
}

/// Remove our entries for `event`; empty containers are removed too.
fn drop_hook(doc: &mut Value, event: &str, mark: &str) -> bool {
    let Some(arr) = doc["hooks"][event].as_array_mut() else {
        return false;
    };
    let before = arr.len();
    arr.retain(|e| !is_ours(e, mark));
    let changed = arr.len() != before;
    if arr.is_empty() {
        if let Some(h) = doc["hooks"].as_object_mut() {
            h.remove(event);
            if h.is_empty() {
                doc.as_object_mut().map(|o| o.remove("hooks"));
            }
        }
    }
    changed
}

fn read_settings(ctx: &Ctx) -> Result<Value, String> {
    let path = settings_path(ctx);
    match std::fs::read_to_string(&path) {
        Err(_) => Ok(json!({})),
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str::<Value>(&s)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| format!("{} is not a JSON object; left untouched", path.display())),
    }
}

fn write_settings(doc: &Value, ctx: &Ctx) -> Step {
    let path = settings_path(ctx);
    backup_once(&path, ctx);
    let mut text = serde_json::to_string_pretty(doc).unwrap_or_default();
    text.push('\n');
    write_if_changed(&path, &text, ctx)
}

fn skill_path(ctx: &Ctx) -> PathBuf {
    ctx.home
        .join(".claude")
        .join("skills")
        .join("cora-code")
        .join("SKILL.md")
}

fn claude_md(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".claude").join("CLAUDE.md")
}

/// Is a `cora` MCP server registered with Claude Code already?
fn mcp_registered() -> Option<bool> {
    let out = std::process::Command::new("claude")
        .args(["mcp", "get", "cora"])
        .output()
        .ok()?;
    Some(out.status.success())
}

fn register_mcp(ctx: &Ctx, r: &mut Report) {
    if !ctx.register_mcp {
        r.push("mcp", Step::Skipped("not requested".into()));
        return;
    }
    if which::which("claude").is_err() {
        r.push("mcp", Step::Skipped("`claude` not on PATH".into()));
        r.todo.push(format!(
            "register the MCP server: claude mcp add-json --scope user cora '{{\"type\":\"stdio\",\"command\":\"{}\",\"args\":[\"mcp\"],\"alwaysLoad\":true}}'",
            ctx.cora
        ));
        return;
    }
    if mcp_registered() == Some(true) && !ctx.force {
        r.push("mcp", Step::Unchanged("already registered".into()));
        return;
    }
    if ctx.dry_run {
        r.push(
            "mcp",
            Step::Done("would register `cora` (alwaysLoad)".into()),
        );
        return;
    }
    if mcp_registered() == Some(true) {
        let _ = std::process::Command::new("claude")
            .args(["mcp", "remove", "--scope", "user", "cora"])
            .output();
    }
    let spec =
        json!({"type":"stdio","command":ctx.cora,"args":["mcp"],"alwaysLoad":true}).to_string();
    match std::process::Command::new("claude")
        .args(["mcp", "add-json", "--scope", "user", "cora", &spec])
        .output()
    {
        Ok(o) if o.status.success() => {
            r.push("mcp", Step::Done("registered `cora` (alwaysLoad)".into()))
        }
        Ok(o) => r.push(
            "mcp",
            Step::Failed(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        ),
        Err(e) => r.push("mcp", Step::Failed(e.to_string())),
    }
}

impl Integration for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn detected(&self, ctx: &Ctx) -> bool {
        ctx.home.join(".claude").is_dir() || which::which("claude").is_ok()
    }

    fn install(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("claude", self.detected(ctx));

        match read_settings(ctx) {
            Err(e) => r.push("hooks", Step::Failed(e)),
            Ok(mut doc) => {
                let p = format!("{} hook context --agent claude", ctx.cora);
                let s = format!("{} hook session --agent claude", ctx.cora);
                let a = set_hook(&mut doc, PROMPT_EVENT, PROMPT_MARK, &p, 10, ctx.force);
                let b = set_hook(&mut doc, SESSION_EVENT, SESSION_MARK, &s, 10, ctx.force);
                r.push(
                    "hooks",
                    if a || b {
                        write_settings(&doc, ctx)
                    } else {
                        Step::Unchanged(settings_path(ctx).display().to_string())
                    },
                );
            }
        }

        register_mcp(ctx, &mut r);
        r.push("skill", write_if_changed(&skill_path(ctx), CODE_SKILL, ctx));
        if ctx.no_instructions {
            r.push("instructions", Step::Skipped("--no-instructions".into()));
        } else {
            r.push(
                "instructions",
                upsert_instruction_block(&claude_md(ctx), INSTRUCTIONS, ctx),
            );
        }
        r
    }

    fn remove(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("claude", self.detected(ctx));
        match read_settings(ctx) {
            Err(e) => r.push("hooks", Step::Failed(e)),
            Ok(mut doc) => {
                let a = drop_hook(&mut doc, PROMPT_EVENT, PROMPT_MARK);
                let b = drop_hook(&mut doc, SESSION_EVENT, SESSION_MARK);
                r.push(
                    "hooks",
                    if a || b {
                        write_settings(&doc, ctx)
                    } else {
                        Step::Unchanged("no cora hooks".into())
                    },
                );
            }
        }
        if ctx.register_mcp && which::which("claude").is_ok() && mcp_registered() == Some(true) {
            if ctx.dry_run {
                r.push("mcp", Step::Done("would remove `cora`".into()));
            } else {
                let ok = std::process::Command::new("claude")
                    .args(["mcp", "remove", "--scope", "user", "cora"])
                    .output()
                    .is_ok_and(|o| o.status.success());
                r.push(
                    "mcp",
                    if ok {
                        Step::Done("removed `cora`".into())
                    } else {
                        Step::Failed("claude mcp remove failed".into())
                    },
                );
            }
        }
        r.push("skill", remove_file_step(&skill_path(ctx), ctx));
        r.push(
            "instructions",
            remove_instruction_block(&claude_md(ctx), ctx),
        );
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
            register_mcp: false,
            cora: "cora".into(),
        }
    }

    #[test]
    fn hooks_are_merged_once_and_keep_the_users_own() {
        let mut doc = json!({"model":"x","hooks":{"UserPromptSubmit":[hook_entry("mine", 5)]}});
        assert!(set_hook(
            &mut doc,
            PROMPT_EVENT,
            PROMPT_MARK,
            "cora hook context --agent claude",
            10,
            false
        ));
        assert!(!set_hook(
            &mut doc,
            PROMPT_EVENT,
            PROMPT_MARK,
            "cora hook context --agent claude",
            10,
            false
        ));
        assert_eq!(doc["hooks"][PROMPT_EVENT].as_array().unwrap().len(), 2);
        assert_eq!(doc["model"], "x");
        assert!(drop_hook(&mut doc, PROMPT_EVENT, PROMPT_MARK));
        assert_eq!(doc["hooks"][PROMPT_EVENT].as_array().unwrap().len(), 1);
        assert_eq!(doc["hooks"][PROMPT_EVENT][0]["hooks"][0]["command"], "mine");
    }

    #[test]
    fn removing_the_last_hook_removes_empty_containers() {
        let mut doc = json!({});
        set_hook(
            &mut doc,
            SESSION_EVENT,
            SESSION_MARK,
            "cora hook session --agent claude",
            10,
            false,
        );
        drop_hook(&mut doc, SESSION_EVENT, SESSION_MARK);
        assert_eq!(doc, json!({}));
    }

    #[test]
    fn a_changed_command_is_replaced_only_with_force() {
        let mut doc = json!({});
        set_hook(
            &mut doc,
            PROMPT_EVENT,
            PROMPT_MARK,
            "old hook context --agent claude",
            10,
            false,
        );
        assert!(!set_hook(
            &mut doc,
            PROMPT_EVENT,
            PROMPT_MARK,
            "new hook context --agent claude",
            10,
            false
        ));
        assert!(set_hook(
            &mut doc,
            PROMPT_EVENT,
            PROMPT_MARK,
            "new hook context --agent claude",
            10,
            true
        ));
        assert_eq!(
            doc["hooks"][PROMPT_EVENT][0]["hooks"][0]["command"],
            "new hook context --agent claude"
        );
    }

    #[test]
    fn a_settings_file_that_is_not_json_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(settings_path(&c), "{ not json").unwrap();
        let r = Claude.install(&c);
        assert!(r.failed());
        assert_eq!(
            std::fs::read_to_string(settings_path(&c)).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn install_then_remove_leaves_the_users_files_as_they_were() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        let settings = "{\n  \"model\": \"opus\"\n}\n";
        std::fs::write(settings_path(&c), settings).unwrap();
        std::fs::write(claude_md(&c), "# mine\n").unwrap();

        let r = Claude.install(&c);
        assert!(!r.failed(), "{}", r.render());
        let text = std::fs::read_to_string(settings_path(&c)).unwrap();
        assert!(text.contains("hook context --agent claude") && text.contains("\"opus\""));
        assert!(skill_path(&c).exists());
        assert!(
            std::fs::read_to_string(claude_md(&c))
                .unwrap()
                .contains("cora_brain_search")
        );

        // idempotent
        let again = Claude.install(&c);
        assert!(
            again
                .items
                .iter()
                .filter(|i| i.what != "mcp")
                .all(|i| matches!(i.step, Step::Unchanged(_))),
            "{}",
            again.render()
        );

        Claude.remove(&c);
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path(&c)).unwrap()).unwrap();
        assert_eq!(back, json!({"model":"opus"}));
        assert!(!skill_path(&c).exists());
        assert_eq!(std::fs::read_to_string(claude_md(&c)).unwrap(), "# mine\n");
    }
}
