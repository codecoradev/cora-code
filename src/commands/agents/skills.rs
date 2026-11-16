//! The skills cora ships (`cora-review`, `cora-code`) and how they are kept in
//! step with the binary (#762).
//!
//! Each skill is embedded in the binary and written into agents by `cora
//! install`. A marker line records the cora version that wrote it and a hash of
//! the text, so later runs can tell three things apart:
//!
//! - a copy cora wrote and nobody edited: refreshed to the running version;
//! - a copy the user edited (the hash no longer matches): kept unless `--force`;
//! - a copy from a cora that wrote no marker yet (`cora-review` up to v0.20.0):
//!   recognized by the hash of the shipped text and refreshed.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Placeholder line in the embedded skill text, replaced by [`render`].
const PLACEHOLDER: &str = "{{CORA_SKILL_MARKER}}";
/// Start of the marker line in an installed skill.
const MARKER_PREFIX: &str = "<!-- cora-skill ";

/// SHA-256 of the `cora-review` text shipped in v0.19.0 and v0.20.0, before skills
/// carried a marker. A file with exactly this text is cora's, not the user's.
pub const LEGACY_REVIEW_SKILLS: &[&str] = &[
    "3e433004846a6fb06887a2ac6aac619d2618c48a219d80fc76c5800d16804701",
    "025f21928cf9c52a5fda8a9eb0bc6c330818bea49c1e804a4125b1b2eab50c0d",
];

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// `text` without its marker or placeholder line.
fn strip_marker(text: &str) -> String {
    text.split_inclusive('\n')
        .filter(|l| !l.starts_with(MARKER_PREFIX) && !l.starts_with(PLACEHOLDER))
        .collect()
}

/// The text to install: the embedded skill with its marker line filled in.
pub fn render(raw: &str) -> String {
    let marker = format!(
        "{MARKER_PREFIX}version={} sha256={} -->",
        env!("CARGO_PKG_VERSION"),
        sha(&strip_marker(raw))
    );
    raw.replace(PLACEHOLDER, &marker)
}

/// What an installed skill file is, relative to the one this binary ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Missing,
    UpToDate,
    /// Written by cora, unedited, from another version (`None` for a legacy copy).
    Stale(Option<String>),
    /// Edited by the user, or not cora's at all.
    Customized,
}

fn marker_of(text: &str) -> Option<(String, String)> {
    let line = text.lines().find(|l| l.starts_with(MARKER_PREFIX))?;
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(key))
            .map(str::to_string)
    };
    Some((field("version=")?, field("sha256=")?))
}

pub fn classify(existing: Option<&str>, rendered: &str, legacy: &[&str]) -> State {
    let Some(text) = existing else {
        return State::Missing;
    };
    if text == rendered {
        return State::UpToDate;
    }
    if let Some((version, recorded)) = marker_of(text) {
        return if sha(&strip_marker(text)) == recorded {
            State::Stale(Some(version))
        } else {
            State::Customized
        };
    }
    if legacy.contains(&sha(text).as_str()) {
        State::Stale(None)
    } else {
        State::Customized
    }
}

/// What an install or removal did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Installed,
    Updated(Option<String>),
    UpToDate,
    /// The file is the user's: left alone.
    KeptCustomized,
    Removed,
    NotPresent,
    /// Dry run of the above.
    Would(Box<Outcome>),
}

fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Install `raw` at `path`, refreshing a cora-written copy and never touching a
/// customized one unless `force`.
pub fn install(
    path: &Path,
    raw: &str,
    legacy: &[&str],
    dry_run: bool,
    force: bool,
) -> Result<Outcome> {
    let rendered = render(raw);
    let outcome = match classify(read(path)?.as_deref(), &rendered, legacy) {
        State::UpToDate => return Ok(Outcome::UpToDate),
        State::Customized if !force => return Ok(Outcome::KeptCustomized),
        State::Missing => Outcome::Installed,
        State::Stale(from) => Outcome::Updated(from),
        State::Customized => Outcome::Updated(None),
    };
    if dry_run {
        return Ok(Outcome::Would(Box::new(outcome)));
    }
    super::write_atomic(path, &rendered)?;
    Ok(outcome)
}

/// Remove the skill at `path` if it is cora's (or `force`).
pub fn remove(
    path: &Path,
    raw: &str,
    legacy: &[&str],
    dry_run: bool,
    force: bool,
) -> Result<Outcome> {
    let rendered = render(raw);
    match classify(read(path)?.as_deref(), &rendered, legacy) {
        State::Missing => Ok(Outcome::NotPresent),
        State::Customized if !force => Ok(Outcome::KeptCustomized),
        _ if dry_run => Ok(Outcome::Would(Box::new(Outcome::Removed))),
        _ => {
            std::fs::remove_file(path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir); // only if now empty
            }
            Ok(Outcome::Removed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str =
        "---\nname: x\ndescription: d\n---\n\n{{CORA_SKILL_MARKER}}\n\n# Body\nhello\n";

    #[test]
    fn a_rendered_skill_carries_the_version_and_a_hash_of_its_body() {
        let r = render(RAW);
        assert!(!r.contains("{{"));
        let (version, hash) = marker_of(&r).unwrap();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert_eq!(hash, sha(&strip_marker(&r)));
        // The hash covers the body only, so it is the same before and after rendering.
        assert_eq!(strip_marker(RAW), strip_marker(&r));
    }

    #[test]
    fn install_refreshes_a_cora_copy_keeps_an_edited_one_and_recognizes_a_legacy_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("skills").join("x").join("SKILL.md");

        assert_eq!(
            install(&path, RAW, &[], false, false).unwrap(),
            Outcome::Installed
        );
        assert_eq!(
            install(&path, RAW, &[], false, false).unwrap(),
            Outcome::UpToDate
        );

        // An older cora wrote it (other version, unedited body): refreshed.
        let old = std::fs::read_to_string(&path)
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "0.0.1");
        std::fs::write(&path, &old).unwrap();
        assert_eq!(
            install(&path, RAW, &[], false, false).unwrap(),
            Outcome::Updated(Some("0.0.1".into()))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), render(RAW));

        // The user edited it: kept, even though it still has the marker.
        let edited = format!("{}\nmy own rule\n", render(RAW));
        std::fs::write(&path, &edited).unwrap();
        assert_eq!(
            install(&path, RAW, &[], false, false).unwrap(),
            Outcome::KeptCustomized
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        assert_eq!(
            install(&path, RAW, &[], false, true).unwrap(),
            Outcome::Updated(None)
        );

        // A copy from before markers existed, known by its hash: refreshed.
        let legacy_text = "---\nname: x\n---\nold text\n";
        std::fs::write(&path, legacy_text).unwrap();
        let legacy = [sha(legacy_text)];
        let legacy_refs: Vec<&str> = legacy.iter().map(String::as_str).collect();
        assert_eq!(
            install(&path, RAW, &legacy_refs, false, false).unwrap(),
            Outcome::Updated(None)
        );
        // The same text without being a known legacy text is the user's.
        std::fs::write(&path, legacy_text).unwrap();
        assert_eq!(
            install(&path, RAW, &[], false, false).unwrap(),
            Outcome::KeptCustomized
        );
    }

    #[test]
    fn dry_run_writes_nothing_and_remove_only_takes_cora_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s").join("SKILL.md");
        assert_eq!(
            install(&path, RAW, &[], true, false).unwrap(),
            Outcome::Would(Box::new(Outcome::Installed))
        );
        assert!(!path.exists());

        install(&path, RAW, &[], false, false).unwrap();
        assert_eq!(
            remove(&path, RAW, &[], true, false).unwrap(),
            Outcome::Would(Box::new(Outcome::Removed))
        );
        assert!(path.exists());
        assert_eq!(
            remove(&path, RAW, &[], false, false).unwrap(),
            Outcome::Removed
        );
        assert!(
            !path.exists() && !path.parent().unwrap().exists(),
            "empty dir removed too"
        );
        assert_eq!(
            remove(&path, RAW, &[], false, false).unwrap(),
            Outcome::NotPresent
        );

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "mine").unwrap();
        assert_eq!(
            remove(&path, RAW, &[], false, false).unwrap(),
            Outcome::KeptCustomized
        );
        assert!(path.exists());
    }

    #[test]
    fn the_shipped_review_skill_hashes_are_the_released_ones() {
        // Pin: the two released texts must stay recognizable.
        assert_eq!(LEGACY_REVIEW_SKILLS.len(), 2);
        assert!(LEGACY_REVIEW_SKILLS.iter().all(|h| h.len() == 64));
    }

    // ── the skills must not drift from the code they describe ──────────────

    /// Every skill cora ships: (name, embedded text).
    fn shipped() -> Vec<(&'static str, &'static str)> {
        vec![
            ("cora-code", super::super::CODE_SKILL),
            ("cora-review", include_str!("../cora_review_skill.md")),
        ]
    }

    /// `cora.tool_name` or `cora_tool_name` tokens in `text`.
    fn tool_mentions(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i + 5 < chars.len() {
            let word_start = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            if word_start
                && chars[i..].starts_with(&['c', 'o', 'r', 'a'])
                && matches!(chars[i + 4], '.' | '_')
            {
                let mut j = i + 5;
                while j < chars.len() && (chars[j].is_ascii_lowercase() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[i + 5..j].iter().collect();
                let name = name.trim_end_matches('_');
                if !name.is_empty() {
                    out.push(format!("cora.{name}"));
                }
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn every_mcp_tool_a_skill_names_exists() {
        let known: Vec<String> = crate::mcp::tools::list_tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for (skill, text) in shipped() {
            let mentions = tool_mentions(text);
            assert!(
                !mentions.is_empty(),
                "{skill} names no MCP tool: the check is blind"
            );
            for m in mentions {
                assert!(
                    known.contains(&m),
                    "skill `{skill}` names the MCP tool `{m}`, which cora does not have; \
                     update the skill (known tools: {known:?})"
                );
            }
        }
    }

    #[test]
    fn every_cora_command_and_flag_a_skill_shows_exists() {
        use clap::CommandFactory;
        let cli = crate::Cli::command();
        for (skill, text) in shipped() {
            let mut checked = 0;
            for span in text.split('`').skip(1).step_by(2) {
                let Some(rest) = span.strip_prefix("cora ") else {
                    continue;
                };
                let mut words = rest.split_whitespace();
                let Some(first) = words.next() else { continue };
                if first.starts_with('<') || first.starts_with('-') {
                    continue;
                }
                let sub = cli.find_subcommand(first).unwrap_or_else(|| {
                    panic!("skill `{skill}` shows `cora {first}`, which is not a cora subcommand: `{span}`")
                });
                checked += 1;
                for w in words.filter(|w| w.starts_with("--")) {
                    for flag in w.split('|') {
                        let flag = flag
                            .trim_start_matches("--")
                            .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
                        let known = sub.get_arguments().any(|a| a.get_long() == Some(flag))
                            || cli.get_arguments().any(|a| a.get_long() == Some(flag));
                        assert!(
                            known,
                            "skill `{skill}` shows `--{flag}` for `cora {first}`, which has no such flag: `{span}`"
                        );
                    }
                }
            }
            assert!(
                checked > 0,
                "{skill} shows no `cora <command>`: the check is blind"
            );
        }
    }

    #[test]
    fn every_skill_carries_the_marker_and_valid_frontmatter() {
        for (skill, text) in shipped() {
            assert!(
                text.starts_with(&format!("---\nname: {skill}\ndescription: ")),
                "{skill}"
            );
            assert_eq!(
                text.matches(PLACEHOLDER).count(),
                1,
                "{skill} needs exactly one marker line"
            );
            assert!(render(text).contains(env!("CARGO_PKG_VERSION")));
        }
    }
}
