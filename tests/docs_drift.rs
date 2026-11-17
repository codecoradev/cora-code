//! The documentation must stay true to the code.
//!
//! The same facts live in several files (README, docs/, CONTRIBUTING, AGENT.md)
//! and drift apart: a wrong license, a legacy config path in a dozen places, an
//! installer that skips the checksum. These tests read the files and compare
//! them with the sources of truth (`Cargo.toml`, the MCP server, the language
//! table, the installer scripts), so the next stale statement fails CI instead
//! of reaching a release.

use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

/// Maintained documentation: the root guides and the flat pages of `docs/`.
/// `docs/changelog.md` is generated from CHANGELOG.md by the docs build, and the
/// design notes and standards keep history, so they are not checked.
fn doc_files() -> Vec<String> {
    let mut files: Vec<String> = [
        "README.md",
        "CONTRIBUTING.md",
        "AGENT.md",
        ".agent.md",
        "SECURITY.md",
    ]
    .iter()
    .filter(|f| root().join(f).exists())
    .map(|f| f.to_string())
    .collect();
    let mut pages: Vec<String> = fs::read_dir(root().join("docs"))
        .expect("docs/")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md") && n != "changelog.md")
        .map(|n| format!("docs/{n}"))
        .collect();
    pages.sort();
    files.extend(pages);
    files
}

fn cargo_toml() -> String {
    read("Cargo.toml")
}

/// A value of `key = "value"` in the `[package]` table.
fn cargo_package_value(key: &str) -> String {
    let re = Regex::new(&format!(r#"(?m)^{key}\s*=\s*"([^"]+)""#)).unwrap();
    re.captures(&cargo_toml())
        .unwrap_or_else(|| panic!("Cargo.toml has no {key}"))[1]
        .to_string()
}

/// The text of a file without fenced code blocks.
fn without_fences(text: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn no_document_names_the_legacy_global_config_directory() {
    // The data directory is ~/.codecora/cora-code/ (CODECORA_HOME overrides it);
    // ~/.cora/ is only migrated from, once.
    for file in doc_files() {
        let text = read(&file);
        assert!(
            !text.contains("~/.cora/"),
            "{file} names the legacy ~/.cora/ directory; use ~/.codecora/cora-code/"
        );
    }
}

#[test]
fn version_pins_in_the_install_docs_match_cargo_toml() {
    let version = format!("v{}", cargo_package_value("version"));
    let re = Regex::new(r"(?:CORA_)?VERSION=(v\d+\.\d+\.\d+)").unwrap();
    for file in [
        "README.md",
        "docs/installation.md",
        "docs/getting-started.md",
    ] {
        let pins: Vec<String> = re
            .captures_iter(&read(file))
            .map(|c| c[1].to_string())
            .collect();
        for pin in &pins {
            assert_eq!(
                pin, &version,
                "{file} pins {pin}, Cargo.toml is {version}: update the pin in the release commit"
            );
        }
    }
    for file in ["README.md", "docs/installation.md"] {
        assert!(
            re.is_match(&read(file)),
            "{file} no longer shows how to pin a version"
        );
    }
}

#[test]
fn the_readme_license_is_the_one_in_cargo_toml() {
    let license = cargo_package_value("license");
    let readme = read("README.md");
    let section = readme
        .split("## License")
        .nth(1)
        .expect("README has no License section");
    assert!(
        section.contains(&license),
        "README License section does not say {license}"
    );
    assert!(
        readme.contains(&format!("License-{}", license.replace('-', "_"))),
        "README badge does not say {license}"
    );
    let file = read("LICENSE");
    let expected = if license.starts_with("Apache") {
        "Apache License"
    } else {
        license.as_str()
    };
    assert!(file.contains(expected), "LICENSE is not the {license} text");
}

#[test]
fn the_readme_rust_badge_matches_rust_version() {
    let msrv = cargo_package_value("rust-version");
    assert!(
        read("README.md").contains(&format!("Rust-{msrv}+")),
        "README Rust badge does not say {msrv}+ (Cargo.toml rust-version)"
    );
}

#[test]
fn documents_do_not_call_a_default_feature_opt_in() {
    let manifest = cargo_toml();
    let default = Regex::new(r"(?m)^default\s*=\s*\[([^\]]*)\]")
        .unwrap()
        .captures(&manifest)
        .expect("Cargo.toml has no default features")[1]
        .to_string();
    let features: Vec<String> = default
        .split(',')
        .map(|f| f.trim().trim_matches('"').to_lowercase())
        .filter(|f| !f.is_empty())
        .collect();
    assert!(!features.is_empty());
    for file in doc_files() {
        for (n, line) in read(&file).lines().enumerate() {
            let lower = line.to_lowercase();
            for feature in &features {
                assert!(
                    !(lower.contains(feature) && lower.contains("opt-in")),
                    "{file}:{}: calls the default feature `{feature}` opt-in",
                    n + 1
                );
            }
        }
    }
}

/// (all languages, languages with a Tree-sitter AST) from the language table.
fn language_table_counts() -> (usize, usize) {
    let text = read("docs/code-intelligence.md");
    let section = text
        .split("### Supported Languages")
        .nth(1)
        .expect("docs/code-intelligence.md has no Supported Languages section");
    let mut total = 0;
    let mut ast = 0;
    for line in section.lines() {
        if line.starts_with("| **") {
            total += 1;
            if line.trim_end_matches('|').trim_end().ends_with('✅') || line.contains("✅ (via") {
                ast += 1;
            }
        } else if total > 0 && !line.starts_with('|') {
            break;
        }
    }
    (total, ast)
}

#[test]
fn language_counts_match_the_language_table() {
    let (total, ast) = language_table_counts();
    assert!(
        total >= 10 && ast >= 5 && ast <= total,
        "table {total}/{ast}"
    );
    let re = Regex::new(r"(\d+) languages").unwrap();
    for file in doc_files() {
        // The roadmap keeps history ("4 -> 12 languages").
        if file == "docs/roadmap.md" {
            continue;
        }
        for (n, line) in read(&file).lines().enumerate() {
            let lower = line.to_lowercase();
            for cap in re.captures_iter(line) {
                let said: usize = cap[1].parse().unwrap();
                let expected = if lower.contains("tree-sitter") || lower.contains("ast") {
                    ast
                } else if lower.contains("symbols") || lower.contains("regex") {
                    total
                } else {
                    continue;
                };
                assert_eq!(
                    said,
                    expected,
                    "{file}:{}: says {said} languages, the table in docs/code-intelligence.md has {expected}",
                    n + 1
                );
            }
        }
    }
}

/// The number of tools `cora mcp` really lists.
fn mcp_tool_count() -> usize {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_cora"))
        .arg("mcp")
        .current_dir(home.path())
        .env("CODECORA_HOME", home.path())
        .env("CORA_EMBED_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cora mcp");
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"docs","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["id"] == 2 {
            return v["result"]["tools"].as_array().expect("tools array").len();
        }
    }
    panic!("no tools/list answer from cora mcp");
}

#[test]
fn the_mcp_tool_count_in_the_docs_is_the_servers() {
    let actual = mcp_tool_count();
    assert!(actual > 10, "implausible tool count {actual}");
    let re = Regex::new(r"(\d+) tools").unwrap();
    for file in doc_files() {
        // The roadmap keeps history ("MCP Phase 1 (5 tools)").
        if file == "docs/roadmap.md" {
            continue;
        }
        for (n, line) in read(&file).lines().enumerate() {
            let lower = line.to_lowercase();
            if !(lower.contains("mcp") || lower.contains("tools for ai coding agents")) {
                continue;
            }
            for cap in re.captures_iter(line) {
                let said: usize = cap[1].parse().unwrap();
                assert_eq!(
                    said,
                    actual,
                    "{file}:{}: says {said} MCP tools, `cora mcp` lists {actual}",
                    n + 1
                );
            }
        }
    }
}

#[test]
fn the_installer_the_docs_recommend_verifies_the_download() {
    // A one-line install command is only as safe as the script it names.
    let re = Regex::new(r"main/(install[\w.-]*\.sh)").unwrap();
    let mut seen = HashSet::new();
    for file in doc_files() {
        for cap in re.captures_iter(&read(&file)) {
            seen.insert(cap[1].to_string());
        }
    }
    assert!(seen.contains("install.sh"), "no document shows install.sh");
    for script in seen {
        let text = read(&script);
        assert!(
            text.contains("sha256") && text.contains("checksums"),
            "{script} is recommended in the docs but does not verify the checksum"
        );
    }
}

fn slug(heading: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in heading.trim().chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            for l in c.to_lowercase() {
                out.push(l);
            }
            prev_space = false;
        } else if c == ' ' && !prev_space {
            out.push('-');
            prev_space = true;
        }
    }
    out
}

fn heading_slugs(text: &str) -> HashSet<String> {
    let mut slugs = HashSet::new();
    for line in without_fences(text).lines() {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            slugs.insert(slug(&line[hashes..]));
        }
    }
    slugs
}

#[test]
fn relative_links_and_anchors_resolve() {
    let link = Regex::new(r"\]\(([^)\s]+)\)").unwrap();
    let site = "https://codecora.dev/cora/docs/";
    let mut problems = Vec::new();
    for file in doc_files() {
        let text = read(&file);
        let dir = Path::new(&file)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        for cap in link.captures_iter(&without_fences(&text)) {
            let target = &cap[1];
            let (path, anchor) = match target.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (target, None),
            };
            let resolved: Option<PathBuf> = if let Some(page) = path.strip_prefix(site) {
                let page = page.trim_end_matches('/');
                Some(PathBuf::from(format!(
                    "docs/{}.md",
                    if page.is_empty() { "index" } else { page }
                )))
            } else if path.starts_with("http://")
                || path.starts_with("https://")
                || path.starts_with("mailto:")
            {
                continue;
            } else if path.is_empty() {
                Some(PathBuf::from(&file))
            } else {
                let base = if let Some(abs) = path.strip_prefix('/') {
                    // A site-absolute path is a page or a file of docs/public.
                    let public = PathBuf::from("docs/public").join(abs);
                    if root().join(&public).exists() {
                        public
                    } else {
                        PathBuf::from("docs").join(abs)
                    }
                } else {
                    dir.join(path)
                };
                // VitePress routes drop the .md extension inside docs/.
                if root().join(&base).exists() {
                    Some(base)
                } else if file.starts_with("docs/") {
                    Some(PathBuf::from(format!("{}.md", base.display())))
                } else {
                    Some(base)
                }
            };
            let resolved = resolved.unwrap();
            // docs/changelog.md is generated from CHANGELOG.md by the docs build and
            // not committed, so check the source it is copied from.
            let resolved = if resolved == Path::new("docs/changelog.md") {
                PathBuf::from("CHANGELOG.md")
            } else {
                resolved
            };
            if !root().join(&resolved).exists() {
                problems.push(format!(
                    "{file}: {target} -> {} is missing",
                    resolved.display()
                ));
                continue;
            }
            if let Some(anchor) = anchor {
                // VitePress numbers some anchors (`_07-...`); those are not headings.
                if anchor.starts_with('_') || !resolved.to_string_lossy().ends_with(".md") {
                    continue;
                }
                let slugs = heading_slugs(&read(&resolved.to_string_lossy()));
                if !slugs.contains(anchor) {
                    problems.push(format!(
                        "{file}: {target}: no heading `{anchor}` in {}",
                        resolved.display()
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "broken links:\n{}",
        problems.join("\n")
    );
}

#[test]
fn the_readme_lists_every_page_of_the_docs_site() {
    let config = read("docs/.vitepress/config.mts");
    let readme = read("README.md");
    let re = Regex::new(r"link:\s*'/([a-z\-]+)'").unwrap();
    let mut pages = 0;
    for cap in re.captures_iter(&config) {
        pages += 1;
        let page = &cap[1];
        assert!(
            readme.contains(&format!("codecora.dev/cora/docs/{page})")),
            "README Documentation table does not list the `{page}` page"
        );
    }
    assert!(pages >= 8, "found only {pages} pages in the docs nav");
}

fn diagram_files() -> Vec<PathBuf> {
    let dir = root().join("docs/public/diagrams");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("docs/public/diagrams")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "svg"))
        .collect();
    files.sort();
    files
}

#[test]
fn diagrams_are_accessible_and_self_contained() {
    let files = diagram_files();
    assert!(
        files.len() >= 4,
        "expected the four diagrams, found {}",
        files.len()
    );
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let svg = fs::read_to_string(&file).unwrap();
        assert!(
            svg.contains("<title"),
            "{name}: no <title> for screen readers"
        );
        assert!(svg.contains("<desc"), "{name}: no <desc>");
        assert!(
            svg.contains("prefers-color-scheme:dark") || svg.contains("prefers-color-scheme: dark"),
            "{name}: no dark mode styles"
        );
        for banned in ["<script", "<image", "<foreignObject", "href=\"http"] {
            assert!(!svg.contains(banned), "{name}: contains {banned}");
        }
    }
}

#[test]
fn every_image_has_alt_text_and_every_diagram_is_used() {
    let md_image = Regex::new(r"!\[([^\]]*)\]\(([^)\s]+)\)").unwrap();
    let html_image = Regex::new(r#"<img\b[^>]*>"#).unwrap();
    let html_alt = Regex::new(r#"alt="[^"]+""#).unwrap();
    let mut used = String::new();
    for file in doc_files() {
        let text = read(&file);
        for cap in md_image.captures_iter(&text) {
            assert!(
                !cap[1].trim().is_empty(),
                "{file}: image {} has no alt text",
                &cap[2]
            );
            used.push_str(&cap[2]);
            used.push('\n');
        }
        for tag in html_image.find_iter(&text) {
            let tag = tag.as_str();
            assert!(html_alt.is_match(tag), "{file}: {tag} has no alt text");
            used.push_str(tag);
            used.push('\n');
        }
    }
    for file in diagram_files() {
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        assert!(used.contains(&name), "{name} is not shown in any document");
    }
}

#[test]
fn the_performance_chart_shows_the_numbers_of_the_readme_table() {
    let svg = read("docs/public/diagrams/performance.svg");
    let readme = read("README.md");
    let values = Regex::new(r#"class="val"[^>]*>([\d.]+) s<"#).unwrap();
    let mut seen = 0;
    for cap in values.captures_iter(&svg) {
        seen += 1;
        assert!(
            readme.contains(&format!("**~{} s**", &cap[1])),
            "the performance chart shows {} s, the README table does not",
            &cap[1]
        );
    }
    assert_eq!(seen, 3, "the performance chart should label three bars");
}
