//! `cora hook context` and `cora hook session`: agent hooks that put cora's code
//! search in front of the model before it works (#753).
//!
//! An agent's prompt hook runs a command before the model sees the user's
//! message and adds the command's output to that turn. That is the one
//! deterministic way to get code intelligence used: a skill or an MCP tool
//! depends on the model choosing to call it.
//!
//! The hook is built to be run on every prompt of every session:
//! - it never fails (any problem prints nothing and exits 0, so it can never
//!   block or slow a prompt for long), and it never downloads the model;
//! - it is bounded in time and in output size;
//! - it stays silent for a prompt that is not about code ("ok", "thanks",
//!   slash commands) and for a project that is not indexed;
//! - what it prints is data about the repository: symbol names, locations and
//!   signatures, plus a pointer to the tools.

use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// How an agent wants the context handed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFormat {
    /// Plain text on stdout (Claude Code adds it to the turn).
    Plain,
    /// `{"context": "..."}` (Hermes `pre_llm_call` shell hooks).
    Hermes,
}

impl AgentFormat {
    pub fn parse(s: &str) -> Self {
        match s {
            "hermes" => Self::Hermes,
            _ => Self::Plain,
        }
    }

    /// The stdout for `text` (empty text is still valid output).
    pub fn render(self, text: &str) -> String {
        match self {
            Self::Plain => text.to_string(),
            Self::Hermes => {
                if text.is_empty() {
                    "{}".to_string()
                } else {
                    serde_json::json!({ "context": text }).to_string()
                }
            }
        }
    }
}

/// Settings of one `cora hook context` run.
pub struct ContextOptions {
    pub prompt: Option<String>,
    pub max_chars: usize,
    pub limit: usize,
    pub budget: Duration,
}

/// What the hook was given: the prompt and the directory the agent works in.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HookInput {
    pub prompt: String,
    pub cwd: Option<PathBuf>,
}

/// Read the hook input from stdin text. Agents pass JSON (Claude Code:
/// `prompt` and `cwd`; Hermes: `extra.user_message` and `cwd`); a plain text
/// body is taken as the prompt itself.
pub fn parse_input(raw: &str) -> HookInput {
    let raw = raw.trim();
    if raw.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            let text = |p: &[&str]| -> Option<String> {
                let mut cur = &v;
                for k in p {
                    cur = cur.get(*k)?;
                }
                cur.as_str().map(str::to_string)
            };
            let prompt = text(&["prompt"])
                .or_else(|| text(&["extra", "user_message"]))
                .or_else(|| text(&["user_message"]))
                .or_else(|| text(&["message"]))
                .unwrap_or_default();
            let cwd = text(&["cwd"]).map(PathBuf::from);
            return HookInput { prompt, cwd };
        }
    }
    HookInput {
        prompt: raw.to_string(),
        cwd: None,
    }
}

/// Words that make a short message a reply, not a question about code.
const REPLIES: &[&str] = &[
    "ok", "okay", "yes", "no", "yep", "nope", "sure", "thanks", "thank", "thx", "continue", "go",
    "proceed", "done", "stop", "retry", "again", "next", "lgtm", "good", "great", "nice", "cool",
    "please", "y", "n",
];

/// Does the prompt look like it is about code, so that looking up symbols is
/// worth the time? Slash commands, short replies and chatter are not.
pub fn is_about_code(prompt: &str) -> bool {
    let p = prompt.trim();
    if p.len() < 16 || p.starts_with('/') || p.starts_with('!') {
        return false;
    }
    let words: Vec<&str> = p.split_whitespace().collect();
    let content = words
        .iter()
        .filter(|w| {
            let w = w
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            !w.is_empty() && !REPLIES.contains(&w.as_str())
        })
        .count();
    let has_identifier = words.iter().any(|w| {
        w.contains('_')
            || w.contains("::")
            || w.contains('/')
            || w.contains('.') && w.len() > 4
            || w.chars().skip(1).any(char::is_uppercase)
    });
    content >= 3 || has_identifier
}

fn clip(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let cut: String = one_line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The tools an agent can use to go further, named as they usually appear.
const POINTER: &str = "Go further with cora before you edit: cora_brain_search (ask in words), \
cora_search_symbols (known name), cora_find_callers and cora_find_impact (blast radius of a \
change), cora_find_affected_tests; or the CLI: cora brain|explore|callers|impact.";

/// Format the lookup as the block the model sees.
pub fn format_block(results: &[crate::index::brain::BrainResult], max_chars: usize) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "[cora] Code in this project that matches your request (by meaning, from the local index):\n",
    );
    for r in results {
        let sig = clip(&r.signature, 110);
        let line = if sig.is_empty() || sig == r.name {
            format!("- {} {} ({}:{})\n", r.kind, r.name, r.file, r.line)
        } else {
            format!("- {} ({}:{}) {}\n", r.name, r.file, r.line, sig)
        };
        if out.len() + line.len() + POINTER.len() + 2 > max_chars {
            break;
        }
        out.push_str(&line);
    }
    out.push_str(POINTER);
    out
}

/// Run the search for `prompt` in the project containing `cwd`. Never panics
/// the caller: any failure is an empty string.
fn lookup(prompt: &str, cwd: &std::path::Path, limit: usize, max_chars: usize) -> String {
    // Never fetch the embedding model from inside a hook: it must be fast.
    crate::embed::forbid_download();
    let bridge = crate::engine::index_bridge::IndexBridge::open(cwd);
    let Some((conn, project_id)) = bridge.parts() else {
        return String::new();
    };
    crate::index::session::configure_for_search(
        crate::index::session::ConfigSource::ProjectOnlyAt(bridge.root()),
    );
    let question: String = prompt.chars().take(400).collect();
    let Ok(report) =
        crate::index::brain::brain_search_report(conn, project_id, &question, limit, true)
    else {
        return String::new();
    };
    // Without a keyword or vector signal the list is only graph neighbours of
    // nothing: say nothing.
    if report.diagnostics.fts_hits == 0 && report.diagnostics.vector_hits == 0 {
        return String::new();
    }
    format_block(&report.results, max_chars)
}

/// `cora hook context`: print the context for the prompt (stdin or `--prompt`).
pub fn run_context(agent: AgentFormat, opts: &ContextOptions) -> i32 {
    let mut raw = String::new();
    if opts.prompt.is_none() {
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut raw);
    }
    let mut input = parse_input(&raw);
    if let Some(p) = &opts.prompt {
        input.prompt.clone_from(p);
    }
    let cwd = input
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    let text = if is_about_code(&input.prompt) {
        let (tx, rx) = mpsc::channel();
        let (prompt, limit, max) = (input.prompt.clone(), opts.limit, opts.max_chars);
        std::thread::spawn(move || {
            let _ = tx.send(lookup(&prompt, &cwd, limit, max));
        });
        // On a timeout the thread is left to the process exit.
        rx.recv_timeout(opts.budget).unwrap_or_default()
    } else {
        String::new()
    };
    println!("{}", agent.render(&text));
    0
}

/// The reminder printed at session start.
pub fn session_text(indexed_symbols: Option<i64>) -> String {
    match indexed_symbols {
        Some(n) => format!(
            "[cora] Code intelligence is available for this project ({n} symbols indexed). \
Before you read or change code, search it by meaning instead of guessing file names: \
cora_brain_search for \"where do we ...\" questions, cora_search_symbols for a known name, \
cora_find_callers and cora_find_impact before changing a function, cora_find_affected_tests \
to pick tests. CLI: cora brain|explore|callers|impact. Relevant symbols are added to your \
prompts automatically."
        ),
        None => "[cora] This project has no code index yet. Run `cora index` in the project root (seconds) \
to enable semantic code search (cora_brain_search, cora_find_callers, cora_find_impact) and \
automatic code context on your prompts."
            .to_string(),
    }
}

/// `cora hook session`: print the session-start reminder.
pub fn run_session(agent: AgentFormat) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut raw);
    let cwd = parse_input(&raw)
        .cwd
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let bridge = crate::engine::index_bridge::IndexBridge::open(&cwd);
    let symbols = bridge.parts().and_then(|(conn, pid)| {
        conn.query_row(
            "SELECT COUNT(*) FROM symbols WHERE project_id = ?1",
            [pid],
            |r| r.get::<_, i64>(0),
        )
        .ok()
    });
    let symbols = symbols.filter(|n| *n > 0);
    println!("{}", agent.render(&session_text(symbols)));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::brain::BrainResult;

    fn result(name: &str, sig: &str) -> BrainResult {
        BrainResult {
            symbol_id: 1,
            name: name.to_string(),
            kind: "function".to_string(),
            file: "src/a.rs".to_string(),
            line: 10,
            signature: sig.to_string(),
            score: 0.1,
            signals: vec!["fts".to_string()],
        }
    }

    #[test]
    fn claude_and_hermes_input_shapes_are_read() {
        let claude = r#"{"session_id":"s","cwd":"/work/p","hook_event_name":"UserPromptSubmit","prompt":"where is the retry logic"}"#;
        assert_eq!(
            parse_input(claude),
            HookInput {
                prompt: "where is the retry logic".to_string(),
                cwd: Some("/work/p".into())
            }
        );
        let hermes = r#"{"hook_event_name":"pre_llm_call","cwd":"/w","extra":{"user_message":"fix the parser"}}"#;
        let h = parse_input(hermes);
        assert_eq!(h.prompt, "fix the parser");
        assert_eq!(h.cwd.as_deref(), Some(std::path::Path::new("/w")));
        // plain text and garbage do not panic
        assert_eq!(parse_input("just words").prompt, "just words");
        assert_eq!(parse_input("{not json").prompt, "{not json");
        assert_eq!(parse_input("").prompt, "");
    }

    #[test]
    fn prompts_that_are_not_about_code_are_skipped() {
        for p in [
            "ok",
            "thanks!",
            "yes continue",
            "/help",
            "!ls",
            "go ahead please",
            "   ",
        ] {
            assert!(!is_about_code(p), "{p:?}");
        }
        for p in [
            "where do we retry a failed request",
            "fix the failing test in chunker_tests",
            "rename parse_findings everywhere",
            "src/engine/judge.rs has a bug near admit",
        ] {
            assert!(is_about_code(p), "{p:?}");
        }
    }

    #[test]
    fn the_block_lists_symbols_with_locations_and_points_to_the_tools() {
        let b = format_block(
            &[result("retry_halves", "fn retry_halves(c: &Chunk)")],
            1500,
        );
        assert!(
            b.contains("retry_halves (src/a.rs:10) fn retry_halves(c: &Chunk)"),
            "{b}"
        );
        assert!(b.contains("cora_find_callers"), "{b}");
        assert!(format_block(&[], 1500).is_empty());
    }

    #[test]
    fn the_block_respects_the_size_budget_and_keeps_the_pointer() {
        let many: Vec<BrainResult> = (0..40)
            .map(|i| result(&format!("symbol_number_{i}"), &"x".repeat(100)))
            .collect();
        let b = format_block(&many, 600);
        assert!(b.len() <= 600, "{}", b.len());
        assert!(
            b.contains("cora_brain_search"),
            "the pointer survives the cut"
        );
    }

    #[test]
    fn hermes_gets_json_and_an_empty_answer_is_an_empty_object() {
        assert_eq!(AgentFormat::Hermes.render(""), "{}");
        let j: Value = serde_json::from_str(&AgentFormat::Hermes.render("hello")).unwrap();
        assert_eq!(j["context"], "hello");
        assert_eq!(AgentFormat::Plain.render("hello"), "hello");
        assert_eq!(AgentFormat::parse("hermes"), AgentFormat::Hermes);
        assert_eq!(AgentFormat::parse("claude"), AgentFormat::Plain);
    }

    #[test]
    fn the_session_text_tells_an_unindexed_project_how_to_enable_search() {
        assert!(session_text(None).contains("cora index"));
        assert!(session_text(Some(1234)).contains("1234 symbols"));
    }
}
