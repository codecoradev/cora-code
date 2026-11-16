//! pi and omp (oh-my-pi): a `before_agent_start` extension that injects the
//! `cora hook context` block before each prompt, the MCP server (listed
//! directly, not hidden behind pi's `codemode`) and the `cora-code` skill.
//! Both agents share the extension API shape, so one template serves both.

use super::{
    CODE_SKILL, Ctx, Integration, Report, Step, read_existing, remove_file_step, write_if_changed,
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// First line of a file cora wrote, so it is never mistaken for the user's.
const MARK: &str = "// cora:managed (cora install); remove with `cora install --remove`";

/// The two agents differ only by name and directories.
pub struct PiFamily {
    pub name: &'static str,
    /// Directory under the home directory that holds the agent's files.
    dir: &'static str,
    /// Executable name, to detect the agent.
    exe: &'static str,
    /// Shape of the `mcpServers.cora` entry.
    mcp: fn(&Ctx) -> Value,
}

pub const PI: PiFamily = PiFamily {
    name: "pi",
    dir: ".pi/agent",
    exe: "pi",
    mcp: |ctx| {
        json!({
            "command": ctx.cora,
            "args": ["mcp"],
            // pi hides MCP tools behind `codemode` unless they are `direct`.
            "exposure": "direct",
            "description": "Semantic code search, callers and impact analysis",
            "timeout": 60
        })
    },
};

pub const OMP: PiFamily = PiFamily {
    name: "omp",
    dir: ".omp/agent",
    exe: "omp",
    mcp: |ctx| {
        json!({
            "type": "stdio",
            "command": ctx.cora,
            "args": ["mcp"],
            "timeout": 30000
        })
    },
};

fn extension_source(ctx: &Ctx) -> String {
    let cora = serde_json::to_string(&ctx.cora).unwrap_or_else(|_| "\"cora\"".into());
    format!(
        r#"{MARK}
// Puts the code the user's prompt is about (from cora's local index) in front of
// the model before it answers. Silent for chit-chat and unindexed projects.
const CORA = {cora};

export default function (pi: any) {{
  pi.on("before_agent_start", async (event: any) => {{
    if (event.messageType && event.messageType !== "user") return;
    const prompt = typeof event.prompt === "string" ? event.prompt : "";
    if (!prompt.trim()) return;
    try {{
      const r = await pi.exec(CORA, ["hook", "context", "--prompt", prompt], {{ timeout: 15000 }});
      const out = String(r.stdout ?? "").trim();
      if (r.code !== 0 || !out) return;
      return {{ message: {{ customType: "cora-context", content: out, display: false }} }};
    }} catch {{
      return;
    }}
  }});
}}
"#
    )
}

impl PiFamily {
    fn root(&self, ctx: &Ctx) -> PathBuf {
        ctx.home.join(self.dir)
    }
    fn extension(&self, ctx: &Ctx) -> PathBuf {
        self.root(ctx).join("extensions").join("cora.ts")
    }
    fn mcp_file(&self, ctx: &Ctx) -> PathBuf {
        self.root(ctx).join("mcp.json")
    }
    fn skill(&self, ctx: &Ctx) -> PathBuf {
        self.root(ctx)
            .join("skills")
            .join("cora-code")
            .join("SKILL.md")
    }

    fn install_extension(&self, ctx: &Ctx) -> Step {
        let path = self.extension(ctx);
        match read_existing(&path) {
            Err(e) => return Step::Failed(e),
            Ok(Some(existing)) if !existing.starts_with(MARK) && !ctx.force => {
                return Step::Skipped(format!(
                    "{} exists and is not managed by cora (use --force to replace it)",
                    path.display()
                ));
            }
            Ok(_) => {}
        }
        write_if_changed(&path, &extension_source(ctx), ctx)
    }

    fn install_mcp(&self, ctx: &Ctx) -> Step {
        if !ctx.register_mcp {
            return Step::Skipped("not requested".into());
        }
        let path = self.mcp_file(ctx);
        let mut doc: Value = match read_existing(&path) {
            Err(e) => return Step::Failed(e),
            Ok(Some(s)) if !s.trim().is_empty() => match serde_json::from_str(&s) {
                Ok(v @ Value::Object(_)) => v,
                _ => {
                    return Step::Failed(format!(
                        "{} is not a JSON object; left untouched",
                        path.display()
                    ));
                }
            },
            Ok(_) => json!({}),
        };
        if doc.get("mcpServers").is_some_and(|m| !m.is_object()) {
            return Step::Failed(format!(
                "{}: `mcpServers` is not an object; left untouched",
                path.display()
            ));
        }
        let wanted = (self.mcp)(ctx);
        let servers = &mut doc["mcpServers"];
        if !servers.is_object() {
            *servers = json!({});
        }
        match servers.get("cora") {
            Some(v) if *v == wanted => return Step::Unchanged(path.display().to_string()),
            Some(_) if !ctx.force => {
                return Step::Skipped(format!(
                    "{} already has a different `cora` server (use --force to replace it)",
                    path.display()
                ));
            }
            _ => {}
        }
        servers["cora"] = wanted;
        super::backup_once(&path, ctx);
        let mut text = serde_json::to_string_pretty(&doc).unwrap_or_default();
        text.push('\n');
        write_if_changed(&path, &text, ctx)
    }

    fn remove_mcp(&self, ctx: &Ctx) -> Step {
        let path = self.mcp_file(ctx);
        let text = match read_existing(&path) {
            Ok(Some(t)) => t,
            Ok(None) => return Step::Unchanged(format!("{} not present", path.display())),
            Err(e) => return Step::Failed(e),
        };
        let Ok(mut doc) = serde_json::from_str::<Value>(&text) else {
            return Step::Failed(format!("{} is not JSON; left untouched", path.display()));
        };
        // `get_mut`, not indexing: indexing a missing key inserts `null`.
        let removed = doc
            .get_mut("mcpServers")
            .and_then(Value::as_object_mut)
            .is_some_and(|m| m.remove("cora").is_some());
        if !removed {
            return Step::Unchanged("no `cora` server".into());
        }
        if doc["mcpServers"].as_object().is_some_and(|m| m.is_empty()) {
            doc.as_object_mut().map(|o| o.remove("mcpServers"));
        }
        if ctx.dry_run {
            return Step::Done(format!("would update {}", path.display()));
        }
        if doc.as_object().is_some_and(|o| o.is_empty()) {
            return match std::fs::remove_file(&path) {
                Ok(()) => Step::Done(format!("removed {} (it only held cora)", path.display())),
                Err(e) => Step::Failed(format!("{}: {e}", path.display())),
            };
        }
        let mut out = serde_json::to_string_pretty(&doc).unwrap_or_default();
        out.push('\n');
        write_if_changed(&path, &out, ctx)
    }
}

impl Integration for PiFamily {
    fn name(&self) -> &'static str {
        self.name
    }

    fn detected(&self, ctx: &Ctx) -> bool {
        ctx.home
            .join(self.dir.split('/').next().unwrap_or(self.dir))
            .is_dir()
            || which::which(self.exe).is_ok()
    }

    fn install(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new(self.name, self.detected(ctx));
        r.push("extension", self.install_extension(ctx));
        r.push("mcp", self.install_mcp(ctx));
        r.push("skill", write_if_changed(&self.skill(ctx), CODE_SKILL, ctx));
        r.todo.push(format!(
            "restart {} (or reload extensions) to pick up the extension",
            self.name
        ));
        r
    }

    fn remove(&self, ctx: &Ctx) -> Report {
        let mut r = Report::new(self.name, self.detected(ctx));
        let path = self.extension(ctx);
        match read_existing(&path) {
            Err(e) => r.push("extension", Step::Failed(e)),
            Ok(Some(s)) if !s.starts_with(MARK) && !ctx.force => r.push(
                "extension",
                Step::Skipped(format!("{} is not managed by cora, kept", path.display())),
            ),
            Ok(_) => r.push("extension", remove_file_step(&path, ctx)),
        }
        r.push("mcp", self.remove_mcp(ctx));
        r.push("skill", remove_file_step(&self.skill(ctx), ctx));
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
    fn install_is_idempotent_and_remove_restores_the_users_mcp_file() {
        for fam in [&PI, &OMP] {
            let tmp = tempfile::tempdir().unwrap();
            let c = ctx(tmp.path());
            let mcp = fam.mcp_file(&c);
            std::fs::create_dir_all(mcp.parent().unwrap()).unwrap();
            std::fs::write(&mcp, "{\"mcpServers\":{\"other\":{\"command\":\"x\"}}}").unwrap();

            let r = fam.install(&c);
            assert!(!r.failed(), "{}", r.render());
            let doc: Value = serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
            assert_eq!(doc["mcpServers"]["other"]["command"], "x");
            assert_eq!(doc["mcpServers"]["cora"]["args"][0], "mcp");
            assert!(
                std::fs::read_to_string(fam.extension(&c))
                    .unwrap()
                    .starts_with(MARK)
            );

            let again = fam.install(&c);
            assert!(
                again
                    .items
                    .iter()
                    .all(|i| matches!(i.step, Step::Unchanged(_))),
                "{}",
                again.render()
            );

            fam.remove(&c);
            let back: Value =
                serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
            assert_eq!(back, json!({"mcpServers":{"other":{"command":"x"}}}));
            assert!(!fam.extension(&c).exists() && !fam.skill(&c).exists());
        }
    }

    #[test]
    fn unreadable_or_oddly_shaped_files_are_reported_and_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let mcp = PI.mcp_file(&c);
        let ext = PI.extension(&c);
        std::fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        std::fs::create_dir_all(ext.parent().unwrap()).unwrap();
        std::fs::write(&ext, [0xff, 0xfe]).unwrap(); // not UTF-8
        std::fs::write(&mcp, r#"{"mcpServers":[]}"#).unwrap();

        let r = PI.install(&c);
        assert!(r.failed(), "{}", r.render());
        assert_eq!(std::fs::read(&ext).unwrap(), vec![0xff, 0xfe]);
        assert_eq!(
            std::fs::read_to_string(&mcp).unwrap(),
            r#"{"mcpServers":[]}"#
        );

        let r = PI.remove(&c);
        assert!(
            ext.exists(),
            "an unreadable file is not removed: {}",
            r.render()
        );

        // Removing from a file without `mcpServers` must not insert a null.
        std::fs::write(&mcp, r#"{"theme":"x"}"#).unwrap();
        PI.remove_mcp(&c);
        assert_eq!(std::fs::read_to_string(&mcp).unwrap(), r#"{"theme":"x"}"#);
    }

    #[test]
    fn pi_lists_the_mcp_tools_directly() {
        assert_eq!(
            (PI.mcp)(&ctx(std::path::Path::new("/h")))["exposure"],
            "direct"
        );
    }

    #[test]
    fn a_users_own_extension_with_the_same_name_is_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let path = PI.extension(&c);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "export default function () {}\n").unwrap();
        assert!(matches!(PI.install_extension(&c), Step::Skipped(_)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "export default function () {}\n"
        );
        PI.remove(&c);
        assert!(path.exists(), "an unmanaged file is never removed");
    }

    #[test]
    fn the_extension_embeds_the_binary_path_as_a_string_literal() {
        let mut c = ctx(std::path::Path::new("/h"));
        c.cora = "/opt/my \"cora\"/cora".into();
        let src = extension_source(&c);
        assert!(
            src.contains(r#"const CORA = "/opt/my \"cora\"/cora";"#),
            "{src}"
        );
    }
}
