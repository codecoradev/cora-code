//! opencode: a plugin whose `chat.message` hook appends the `cora hook context`
//! block to the user's prompt, the MCP server (`mcp.cora` in `opencode.json`)
//! and the `cora-code` skill.
//!
//! Written against opencode 1.19.5, whose plugin API is `export const X = async
//! (ctx) => ({ "chat.message": (input, output) => ... })` with the user's text
//! in `output.parts`. The newer `Plugin.define` / `mcp.servers` shapes in the
//! online docs were not found in that release, so they are not generated.

use super::{
    CODE_SKILL, Ctx, Integration, Report, Step, backup_once, read_existing, remove_file_step,
    write_if_changed,
};
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct OpenCode;

const MARK: &str = "// cora:managed (cora install); remove with `cora install --remove`";

fn root(ctx: &Ctx) -> PathBuf {
    ctx.home.join(".config").join("opencode")
}
fn plugin(ctx: &Ctx) -> PathBuf {
    root(ctx).join("plugins").join("cora.js")
}
fn config(ctx: &Ctx) -> PathBuf {
    root(ctx).join("opencode.json")
}
fn skill(ctx: &Ctx) -> PathBuf {
    root(ctx).join("skills").join("cora-code").join("SKILL.md")
}

fn plugin_source(ctx: &Ctx) -> String {
    let cora = serde_json::to_string(&ctx.cora).unwrap_or_else(|_| "\"cora\"".into());
    format!(
        r#"{MARK}
// Appends the code the user's prompt is about (from cora's local index) to the
// prompt before the model sees it. Silent for chit-chat and unindexed projects.
import {{ execFile }} from "node:child_process";

const CORA = {cora};

function context(prompt, cwd) {{
  return new Promise((resolve) => {{
    try {{
      execFile(
        CORA,
        ["hook", "context", "--prompt", prompt],
        {{ cwd, timeout: 15000, encoding: "utf8" }},
        (err, stdout) => resolve(err ? "" : String(stdout ?? "").trim()),
      );
    }} catch {{
      resolve("");
    }}
  }});
}}

export const CoraPlugin = async ({{ directory }}) => ({{
  "chat.message": async (_input, output) => {{
    const part = (output.parts ?? []).find((p) => p.type === "text" && typeof p.text === "string");
    if (!part || !part.text.trim()) return;
    const extra = await context(part.text, directory);
    if (extra) part.text = part.text + "\n\n" + extra;
  }},
}});
"#
    )
}

fn mcp_entry(ctx: &Ctx) -> Value {
    json!({ "type": "local", "command": [ctx.cora, "mcp"], "enabled": true })
}

fn install_mcp(ctx: &Ctx) -> Step {
    if !ctx.register_mcp {
        return Step::Skipped("not requested".into());
    }
    let jsonc = root(ctx).join("opencode.jsonc");
    if jsonc.exists() {
        return Step::Skipped(format!(
            "{} exists (comments allowed): add the server yourself",
            jsonc.display()
        ));
    }
    let path = config(ctx);
    let mut doc: Value = match read_existing(&path) {
        Err(e) => return Step::Failed(e),
        Ok(Some(s)) if !s.trim().is_empty() => match serde_json::from_str(&s) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                return Step::Skipped(format!(
                    "{} is not plain JSON (comments?): add the server yourself",
                    path.display()
                ));
            }
        },
        Ok(_) => json!({ "$schema": "https://opencode.ai/config.json" }),
    };
    if doc.get("mcp").is_some_and(|m| !m.is_object()) {
        return Step::Failed(format!(
            "{}: `mcp` is not an object; left untouched",
            path.display()
        ));
    }
    let wanted = mcp_entry(ctx);
    if !doc["mcp"].is_object() {
        doc["mcp"] = json!({});
    }
    match doc["mcp"].get("cora") {
        Some(v) if *v == wanted => return Step::Unchanged(path.display().to_string()),
        Some(_) if !ctx.force => {
            return Step::Skipped(format!(
                "{} already has a different `mcp.cora` (use --force to replace it)",
                path.display()
            ));
        }
        _ => {}
    }
    doc["mcp"]["cora"] = wanted;
    backup_once(&path, ctx);
    let mut text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    text.push('\n');
    write_if_changed(&path, &text, ctx)
}

fn remove_mcp(ctx: &Ctx) -> Step {
    let path = config(ctx);
    let text = match read_existing(&path) {
        Ok(Some(t)) => t,
        Ok(None) => return Step::Unchanged(format!("{} not present", path.display())),
        Err(e) => return Step::Failed(e),
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&text) else {
        return Step::Skipped(format!(
            "{} is not plain JSON; left untouched",
            path.display()
        ));
    };
    let removed = doc
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .is_some_and(|m| m.remove("cora").is_some());
    if !removed {
        return Step::Unchanged("no `mcp.cora`".into());
    }
    if doc["mcp"].as_object().is_some_and(|m| m.is_empty()) {
        doc.as_object_mut().map(|o| o.remove("mcp"));
    }
    let only_schema = doc
        .as_object()
        .is_some_and(|o| o.keys().all(|k| k == "$schema"));
    if ctx.dry_run {
        return Step::Done(format!("would update {}", path.display()));
    }
    if only_schema {
        return match std::fs::remove_file(&path) {
            Ok(()) => Step::Done(format!("removed {} (it only held cora)", path.display())),
            Err(e) => Step::Failed(format!("{}: {e}", path.display())),
        };
    }
    let mut out = serde_json::to_string_pretty(&doc).unwrap_or_default();
    out.push('\n');
    write_if_changed(&path, &out, ctx)
}

impl Integration for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn detected(&self, ctx: &Ctx) -> bool {
        root(ctx).is_dir() || which::which("opencode").is_ok()
    }

    fn install(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("opencode", self.detected(ctx));
        let path = plugin(ctx);
        let step = match read_existing(&path) {
            Err(e) => Step::Failed(e),
            Ok(Some(s)) if !s.starts_with(MARK) && !ctx.force => Step::Skipped(format!(
                "{} exists and is not managed by cora (use --force to replace it)",
                path.display()
            )),
            Ok(_) => write_if_changed(&path, &plugin_source(ctx), ctx),
        };
        r.push("plugin", step);
        r.push("mcp", install_mcp(ctx));
        r.push("skill", write_if_changed(&skill(ctx), CODE_SKILL, ctx));
        r.todo
            .push("restart opencode to load the plugin".to_string());
        r
    }

    fn remove(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new("opencode", self.detected(ctx));
        let path = plugin(ctx);
        match read_existing(&path) {
            Err(e) => r.push("plugin", Step::Failed(e)),
            Ok(Some(s)) if !s.starts_with(MARK) && !ctx.force => r.push(
                "plugin",
                Step::Skipped(format!("{} is not managed by cora, kept", path.display())),
            ),
            Ok(_) => r.push("plugin", remove_file_step(&path, ctx)),
        }
        r.push("mcp", remove_mcp(ctx));
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
            cora: "cora".into(),
        }
    }

    #[test]
    fn install_is_idempotent_and_remove_restores_the_users_config() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(root(&c)).unwrap();
        std::fs::write(config(&c), "{\"theme\":\"dark\"}").unwrap();

        let r = OpenCode.install(&c);
        assert!(!r.failed(), "{}", r.render());
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(config(&c)).unwrap()).unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(doc["mcp"]["cora"]["command"], json!(["cora", "mcp"]));
        assert!(
            std::fs::read_to_string(plugin(&c))
                .unwrap()
                .starts_with(MARK)
        );

        let again = OpenCode.install(&c);
        assert!(
            again
                .items
                .iter()
                .all(|i| matches!(i.step, Step::Unchanged(_))),
            "{}",
            again.render()
        );

        OpenCode.remove(&c);
        let back: Value =
            serde_json::from_str(&std::fs::read_to_string(config(&c)).unwrap()).unwrap();
        assert_eq!(back, json!({"theme":"dark"}));
        assert!(!plugin(&c).exists() && !skill(&c).exists());
    }

    #[test]
    fn unreadable_or_oddly_shaped_files_are_reported_and_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(plugin(&c).parent().unwrap()).unwrap();
        std::fs::write(plugin(&c), [0xff, 0xfe]).unwrap(); // not UTF-8
        std::fs::write(config(&c), r#"{"mcp":[]}"#).unwrap();

        let r = OpenCode.install(&c);
        assert!(r.failed(), "{}", r.render());
        assert_eq!(std::fs::read(plugin(&c)).unwrap(), vec![0xff, 0xfe]);
        assert_eq!(
            std::fs::read_to_string(config(&c)).unwrap(),
            r#"{"mcp":[]}"#
        );
        OpenCode.remove(&c);
        assert!(plugin(&c).exists(), "an unreadable file is not removed");
    }

    #[test]
    fn a_config_with_comments_is_never_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(root(&c)).unwrap();
        std::fs::write(root(&c).join("opencode.jsonc"), "{ // mine\n}").unwrap();
        let r = OpenCode.install(&c);
        assert!(!r.failed());
        assert!(
            !config(&c).exists(),
            "must not create a competing opencode.json"
        );
        assert!(
            std::fs::read_to_string(root(&c).join("opencode.jsonc"))
                .unwrap()
                .contains("// mine")
        );
    }

    #[test]
    fn a_users_own_plugin_with_the_same_name_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::create_dir_all(plugin(&c).parent().unwrap()).unwrap();
        std::fs::write(plugin(&c), "export const X = async () => ({})\n").unwrap();
        OpenCode.install(&c);
        OpenCode.remove(&c);
        assert_eq!(
            std::fs::read_to_string(plugin(&c)).unwrap(),
            "export const X = async () => ({})\n"
        );
    }
}
