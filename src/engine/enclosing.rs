//! Enclosing-scope context for review prompts (#523).
//!
//! Diff hunks alone cannot answer control-flow claims ("branch X never
//! reaches call Y") — the enclosing `match` arm, its producers, and the
//! preceding if/else usually sit outside the hunk. This module extracts the
//! enclosing function/block from the POST-IMAGE file on disk for hunks that
//! touch branching constructs, bounded to avoid token blowup.

use std::sync::LazyLock;

use crate::engine::diff_parser::{DiffLineType, parse_diff};

/// Max lines of surrounding code emitted per hunk's enclosing block.
pub const MAX_CONTEXT_LINES: usize = 120;

/// Hunks whose ADDED lines touch any of these get enclosing-scope context.
static RE_BRANCHING: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(\bmatch\b|\bif\b|\belse\b|=>|\breturn\b|\.\?|\?\s*;)").unwrap()
});

/// A labeled post-image snippet for one hunk.
#[derive(Debug, Clone)]
pub struct EnclosingSnippet {
    /// File the snippet was read from (post-image path).
    pub file: String,
    /// 1-based inclusive start line in the post-image file.
    pub start_line: usize,
    /// Lines actually emitted.
    pub lines: usize,
}

/// Extract enclosing-scope snippets for a unified diff.
///
/// Only files with a usable post-image on disk are considered (paths are
/// relative to `root`). New files are skipped — their whole content is
/// already inside the diff. One snippet per qualifying hunk, each capped at
/// [`MAX_CONTEXT_LINES`].
pub fn extract_enclosing_snippets(diff: &str, root: &std::path::Path) -> Vec<EnclosingSnippet> {
    let mut out = Vec::new();
    for chunk in parse_diff(diff) {
        if chunk.is_binary || chunk.is_deleted || chunk.is_new {
            continue;
        }
        let Some(path) = &chunk.new_path else {
            continue;
        };

        for hunk in &chunk.chunks {
            // Gate: only spend tokens when branching is involved (#523).
            // A call ADDED inside a shared arm carries the branch structure
            // in its CONTEXT lines (e.g. `Ok(id) => {`), so scan both.
            let touches_branching = hunk
                .lines
                .iter()
                .any(|l| l.line_type != DiffLineType::Remove && RE_BRANCHING.is_match(&l.content));
            if !touches_branching {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(root.join(path)) else {
                continue;
            };
            let lines: Vec<&str> = content.lines().collect();

            // First post-image line inside this hunk.
            let hit = hunk
                .lines
                .iter()
                .filter(|l| l.line_type != DiffLineType::Remove)
                .find_map(|l| l.new_line_no)
                .unwrap_or(hunk.new_start);

            if let Some((start_idx, end_idx)) = enclosing_block_span(&lines, hit as usize) {
                out.push(EnclosingSnippet {
                    file: path.clone(),
                    start_line: start_idx + 1,
                    lines: end_idx - start_idx + 1,
                });
            }
        }
    }
    out
}

/// Render snippets as a prompt section with their post-image code attached.
///
/// Takes a resolver so this module never does I/O twice — the caller reads
/// each snippet's file once and hands over its lines.
pub fn render_for_prompt(
    snippets: &[EnclosingSnippet],
    file_lines: impl Fn(&str) -> Option<Vec<String>>,
) -> String {
    if snippets.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "Surrounding code from the post-image — reference only, NOT part of the diff; \
         verify branch structure here before making reachability claims:\n",
    );
    for sn in snippets {
        let Some(all) = file_lines(&sn.file) else {
            continue;
        };
        let start = sn.start_line.saturating_sub(1);
        let end = (start + sn.lines).min(all.len());
        s.push_str(&format!(
            "=== {} (lines {}-{}) ===\n{}\n\n",
            sn.file,
            start + 1,
            end,
            clamp_context_text(&all[start..end].join("\n"))
        ));
    }
    s
}

/// Find the enclosing definition block around `hit_line` (1-based).
///
/// Naive brace-count heuristic with two passes:
/// 1. walk forward tracking open-brace line numbers; prefer the innermost
///    opener whose line looks like a definition signature (`fn`, `def`,
///    `func`, `function`); fall back to the innermost opener.
/// 2. rescan from that opener to its matching close brace.
///
/// Braces inside string literals may skew counts — acceptable for a
/// best-effort context gate, never used for correctness decisions.
fn enclosing_block_span(lines: &[&str], hit_line: usize) -> Option<(usize, usize)> {
    if lines.is_empty() || hit_line == 0 || hit_line > lines.len() {
        return None;
    }

    const DEF_HINTS: [&str; 4] = ["fn ", "func ", "def ", "function"];

    let mut opens: Vec<(usize, bool)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for c in line.chars() {
            match c {
                '{' => opens.push((i + 1, DEF_HINTS.iter().any(|k| line.contains(k)))),
                '}' => {
                    opens.pop();
                }
                _ => {}
            }
        }
        if i + 1 >= hit_line {
            break;
        }
    }

    let open_line = opens
        .iter()
        .rev()
        .find(|(_, looks_def)| *looks_def)
        .or_else(|| opens.last())
        .map(|(l, _)| *l)?;

    // Second pass: match the braces of the chosen opener.
    let mut depth = 0i64;
    for (i, line) in lines.iter().enumerate().skip(open_line - 1) {
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 {
            return Some((open_line - 1, i));
        }
    }
    Some((open_line - 1, lines.len() - 1))
}

/// Cap rendered content to MAX_CONTEXT_LINES, keeping the head (signature +
/// early producers) and a tail window (shared arms live at the end of long
/// functions).
pub(crate) fn clamp_context_text(text: &str) -> String {
    let count = text.lines().count();
    if count <= MAX_CONTEXT_LINES {
        return text.to_string();
    }
    let head = MAX_CONTEXT_LINES * 2 / 3;
    let tail = MAX_CONTEXT_LINES - head - 1; // minus marker line
    let mut kept: Vec<&str> = text.lines().take(head).collect();
    kept.push("…");
    kept.extend(text.lines().skip(count - tail));
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance fixture (#523): a call added inside a SHARED match arm fed
    /// by two producers. The hunk alone shows only the arm; the enclosing
    /// context must expose both producers so review cannot claim the call is
    /// missing on one branch.
    const FIXTURE: &str = r#"handlers.rs"#;
    const FIXTURE_BODY: &str = r#"use crate::store::Store;

fn remember(store: &mut Store, text: &str) -> usize { store.remember(text) }

fn remember_with_contradiction(store: &mut Store, text: &str) -> (usize, bool) {
    let id = store.remember(text);
    (id, true)
}

fn handle_request(store: &mut Store, detect_contradiction: bool) -> usize {
    let author_type = "user";
    if !author_type.is_empty() { /* validated up front */ }
    let result = if detect_contradiction {
        remember_with_contradiction(store, "note").map(|(id, _)| id)
    } else {
        Ok(remember(store, "note"))
    };
    match result {
        Ok(id) => {
            store.set_author_type(id);
            id
        }
        Err(_) => 0,
    }
}
"#;

    fn write_fixture(dir: &std::path::Path) {
        std::fs::write(dir.join(FIXTURE), FIXTURE_BODY).unwrap();
    }

    fn fixture_diff() -> String {
        // Adds store.set_author_type(id) inside the shared Ok(id) arm.
        let body_line = |n: usize| FIXTURE_BODY.lines().nth(n - 1).unwrap_or("");
        // The added line is at post-image line 22; keep a small hunk window.
        let mut d = String::from("--- a/handlers.rs\n+++ b/handlers.rs\n");
        d.push_str("@@ -19,6 +19,7 @@\n");
        for n in 19..=21 {
            d.push_str(&format!(" {}\n", body_line(n)));
        }
        d.push_str("+            store.set_author_type(id);\n");
        for n in 22..=24 {
            d.push_str(&format!(" {}\n", body_line(n)));
        }
        d
    }

    #[test]
    fn acceptance_shared_match_arm_context_included() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        let snippets = extract_enclosing_snippets(&fixture_diff(), tmp.path());

        assert!(!snippets.is_empty(), "branching hunk must get context");
        assert_eq!(snippets[0].file, FIXTURE);

        let rendered = render_for_prompt(&snippets, |f| {
            std::fs::read_to_string(tmp.path().join(f))
                .map(|c| c.lines().map(String::from).collect())
                .ok()
        });
        // Both producers of `result` are visible next to the shared arm:
        assert!(
            rendered.contains("remember_with_contradiction"),
            "producer 1 must appear in surrounding code"
        );
        assert!(
            rendered.contains("Ok(remember("),
            "producer 2 must appear in surrounding code"
        );
        assert!(
            rendered.contains("set_author_type"),
            "the changed arm itself"
        );
        assert!(rendered.starts_with("Surrounding code"));
    }

    #[test]
    fn no_context_without_branching_lines() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture(tmp.path());
        // Pure arithmetic change — no branching keywords in ADDED lines.
        let diff = "--- a/handlers.rs\n+++ b/handlers.rs\n@@ -2,3 +2,4 @@\n use crate::store::Store;\n+let total = 1 + 2;\n fn remember";
        let snippets = extract_enclosing_snippets(diff, tmp.path());
        assert!(snippets.is_empty(), "no branching → no injection");
    }

    #[test]
    fn new_files_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let diff = "--- /dev/null\n+++ b/newthing.rs\n@@ -0,0 +1,2 @@\n+fn x(a: u8) {\n+    match a { _ => {} }\n}";
        let snippets = extract_enclosing_snippets(diff, tmp.path());
        assert!(snippets.is_empty(), "new file content is already the diff");
    }

    #[test]
    fn long_functions_are_clamped() {
        let big: String = std::iter::once("fn huge() {".to_string())
            .chain((0..400).map(|i| format!("    let v{i} = {i};")))
            .chain(std::iter::once("}".to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("big.rs"), &big).unwrap();
        let diff = format!(
            "--- a/big.rs\n+++ b/big.rs\n@@ -398,4 +398,5 @@\n{}\n+    if v399 {{}}\n}}",
            big.lines().nth(397).unwrap()
        );
        let snippets = extract_enclosing_snippets(&diff, tmp.path());
        assert!(!snippets.is_empty());
        let rendered = render_for_prompt(&snippets, |f| {
            std::fs::read_to_string(tmp.path().join(f))
                .map(|c| c.lines().map(String::from).collect())
                .ok()
        });
        let emitted = rendered.lines().count();
        assert!(
            emitted <= MAX_CONTEXT_LINES + 6,
            "bounded emission expected, got {emitted} lines"
        );
        assert!(rendered.contains('…'), "clamp marker present");
    }

    #[test]
    fn guardrail_rule_is_part_of_prompt() {
        let prompt = super::super::llm::build_review_prompt("d", &[], &[], None, None);
        assert!(
            prompt.contains(crate::engine::llm::CONTROL_FLOW_GUARDRAIL),
            "guardrail text must always ship in review prompts"
        );
    }
}
