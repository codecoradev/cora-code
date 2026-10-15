//! Test-code awareness for the deterministic rules (#663).
//!
//! Tests legitimately do what rules flag: `unwrap()`, `println!`, fake
//! credentials, `http://` URLs. This module says which lines of a file are test
//! code, so noisy rules can skip them:
//!
//! - **path**: `tests/`, `*_test.*`, `*.test.ts`, `spec/`, `fixtures/`, ... a
//!   whole file ([`is_test_path`]);
//! - **inline Rust tests**: `#[cfg(test)]` modules and `#[test]` functions,
//!   found by brace matching ([`rust_test_lines`]);
//! - **diffs**: a hunk whose header (the nearest enclosing item git prints) is
//!   `mod tests` ([`hunk_header_is_test_module`]), since the `#[cfg(test)]`
//!   line itself is usually outside the hunk.
//!
//! The detection is deliberately conservative: when it cannot tell, a line is
//! NOT test code and stays scanned. `rules_engine.include_tests: true` turns the
//! exemptions off.

use std::collections::HashSet;

/// Does `path` look like a test/spec/fixture/mock/example file?
///
/// Uses path-segment awareness so common words like `latest`, `aspect`,
/// `attestation`, `protest` are not mistaken for test files (#87). A segment is
/// any run between `/`, `_`, `-`, and `.` separators.
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.split(['/', '_', '-', '.']).any(|seg| {
        matches!(
            seg,
            "test"
                | "tests"
                | "testing"
                | "tested"
                | "__tests__"
                | "spec"
                | "specs"
                | "fixture"
                | "fixtures"
                | "mock"
                | "mocks"
                | "example"
                | "examples"
        )
    })
}

/// Is the `@@ ... @@ <header>` text of a hunk a test module (`mod tests {`)?
pub fn hunk_header_is_test_module(header: &str) -> bool {
    let h = header.trim();
    let h = h.strip_prefix("pub ").unwrap_or(h);
    h.strip_prefix("mod ").is_some_and(|rest| {
        let name = rest
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        matches!(name, "tests" | "test" | "unit_tests" | "integration_tests")
    })
}

/// Net `{` minus `}` on one line of Rust, ignoring braces inside string and
/// char literals and after `//`. Approximate on purpose (raw strings and
/// multi-line literals are not tracked); an error only shrinks or grows the
/// exempt region, never invents findings.
fn brace_delta(line: &str, in_block: &mut bool) -> i32 {
    let mut delta = 0;
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        // `/* ... */` (may span lines): braces inside are not code.
        if *in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block = false;
            }
            continue;
        }
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            // Single-line raw string `r"..."` / `r#"..."#`: skip its content.
            'r' if matches!(chars.peek(), Some('"') | Some('#')) => {
                let rest: String = chars.clone().collect();
                let hashes = rest.chars().take_while(|&h| h == '#').count();
                if rest[hashes..].starts_with('"') {
                    let close = format!("\"{}", "#".repeat(hashes));
                    if let Some(end) = rest[hashes + 1..].find(&close) {
                        // Consume through the closing delimiter.
                        for _ in 0..(hashes + 1 + end + close.len()) {
                            chars.next();
                        }
                    } else {
                        break; // unterminated on this line: ignore the rest
                    }
                }
            }
            '"' => in_str = true,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                *in_block = true;
            }
            '/' if chars.peek() == Some(&'/') => break,
            '\'' => {
                // Char literal ('{', '\'') vs lifetime ('a): only skip literals.
                let rest: String = chars.clone().take(3).collect();
                if rest.starts_with('\\') {
                    chars.next();
                    chars.next();
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                    }
                } else if rest.chars().nth(1) == Some('\'') {
                    chars.next();
                    chars.next();
                }
            }
            '{' => delta += 1,
            '}' => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// Does this (trimmed) line start a Rust ITEM or macro item (`fn`, `impl`,
/// `pub struct`, `use`, `#[attr]`, `thread_local! {`, ...)? Used by the
/// indentation safety net: content of a multi-line string at column 0 (a TOML
/// or JSON fixture) is not an item and must not abandon a test region, but
/// code that follows the real end of the region is always an item.
fn looks_like_item_start(trimmed: &str) -> bool {
    use std::sync::LazyLock;
    static ITEM_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"^(?:#\[|#!\[|(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const|default|extern(?:\s+"[^"]*")?)\s+)*(?:fn|struct|enum|union|trait|impl|mod|use|type|static|const|extern)\b|macro_rules!\s|[A-Za-z_][\w:]*!\s*[(\[{])"#,
        )
        .expect("item regex must compile")
    });
    ITEM_RE.is_match(trimmed)
}

/// Is this attribute line one that marks the next item as test code?
fn is_test_attribute(trimmed: &str) -> bool {
    trimmed.starts_with("#[cfg(test)]")
        || trimmed.starts_with("#[cfg(all(test")
        || trimmed == "#[test]"
        || (trimmed.starts_with("#[") && trimmed.trim_end().ends_with("::test]"))
        || trimmed.starts_with("#[tokio::test")
        || trimmed.starts_with("#[rstest")
}

/// Line numbers inside Rust test code: from a test attribute through the end of
/// the item it decorates (`#[cfg(test)] mod tests { ... }`, `#[test] fn ...`).
///
/// `lines` are `(line number, text)` in file order. A gap in the numbering (a
/// diff skipping unchanged code) abandons the region being tracked, so a
/// partial view never extends a region it cannot see the end of.
///
/// `partial_view` is true for a diff (the view may end inside a region, whose
/// lines seen so far are still test code). For a WHOLE file it is false: a
/// region that never closes means the brace matching lost track (a raw string,
/// a macro, or a hostile `{`), and exempting everything to the end of the file
/// would hide real code, so the unfinished region is dropped instead.
pub fn rust_test_lines(lines: &[(u32, &str)], partial_view: bool) -> HashSet<u32> {
    let mut out = HashSet::new();
    // (lines of the region so far, brace depth, seen the opening brace,
    // indentation of the decorated item)
    let mut region: Option<(Vec<u32>, i32, bool, usize)> = None;
    let mut in_block = false;
    let mut pending: Vec<u32> = Vec::new();
    let mut prev: Option<u32> = None;

    for &(n, text) in lines {
        if prev.is_some_and(|p| n != p + 1) {
            // Discontinuous: drop what we cannot finish (the lines already
            // collected stay marked: they were inside the region).
            if let Some((seen, _, _, _)) = region.take() {
                out.extend(seen);
            }
            pending.clear();
            in_block = false;
        }
        prev = Some(n);
        let trimmed = text.trim();

        // One comment/brace pass per line, so block-comment state is tracked
        // everywhere (not only inside a region) and cannot go stale.
        let started_in_block = in_block;
        let delta = brace_delta(text, &mut in_block);
        let indent_of_line = text.len() - text.trim_start().len();

        if let Some((seen, depth, opened, indent)) = region.as_mut() {
            // Safety net against a parser differential: inside the region,
            // an ITEM may not start at or left of the decorated item's
            // indentation, and the closing `}` must balance the count. If one
            // does, our brace matching disagrees with the compiler (a block
            // comment, a multi-line string, a hostile `{`), so vouch for
            // nothing. Non-item text at column 0 (a TOML fixture inside a raw
            // string) is fine.
            let closes = !started_in_block && trimmed.starts_with('}');
            if !started_in_block
                && indent_of_line <= *indent
                && !closes
                && looks_like_item_start(trimmed)
            {
                region = None;
                continue;
            }
            seen.push(n);
            *depth += delta;
            if text.contains('{') && !started_in_block {
                *opened = true;
            }
            if closes && indent_of_line <= *indent && *depth > 0 {
                region = None; // the item's own `}` must balance the count
                continue;
            }
            let done = (*opened && *depth <= 0) || (!*opened && trimmed.ends_with(';'));
            if done {
                let (seen, _, _, _) = region.take().expect("region present");
                out.extend(seen);
            }
            continue;
        }

        // Outside a region, a line that begins inside a block comment is
        // comment text: never an attribute, never the decorated item.
        if started_in_block {
            continue;
        }

        let is_attr_line = is_test_attribute(trimmed);
        if is_attr_line || (!pending.is_empty() && trimmed.starts_with("#[")) {
            // `#[cfg(test)] fn helper() {}` carries its item on the same line:
            // the attribute decorates THAT item, not the next line.
            if attribute_rest(trimmed).is_none_or(is_line_comment_or_empty) {
                pending.push(n);
                continue;
            }
        } else if !pending.is_empty() {
            // Blank lines and doc comments between the test attribute and the
            // item keep the region pending.
            if trimmed.is_empty() || trimmed.starts_with("///") {
                pending.push(n);
                continue;
            }
        } else {
            continue;
        }

        // The decorated item starts on this line.
        let mut seen = std::mem::take(&mut pending);
        seen.push(n);
        let opened = text.contains('{');
        if (opened && delta <= 0) || (!opened && trimmed.ends_with(';')) {
            out.extend(seen);
        } else {
            region = Some((seen, delta, opened, indent_of_line));
        }
    }
    if partial_view {
        if let Some((seen, _, _, _)) = region {
            out.extend(seen);
        }
    }
    out
}

/// The text after the first complete `#[...]` attribute on `trimmed`, or
/// `None` when the attribute does not close on this line.
fn attribute_rest(trimmed: &str) -> Option<&str> {
    let bytes = trimmed.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(trimmed[i + 1..].trim());
                }
            }
            _ => {}
        }
    }
    None
}

fn is_line_comment_or_empty(rest: &str) -> bool {
    rest.is_empty() || rest.starts_with("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(src: &str) -> Vec<(u32, &str)> {
        src.lines().zip(1u32..).map(|(t, n)| (n, t)).collect()
    }

    #[test]
    fn path_segments_decide_test_files() {
        for p in [
            "tests/auth_test.py",
            "src/app.test.ts",
            "src/__tests__/setup.rs",
            "spec/models/user_spec.rb",
            "fixtures/a.json",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in [
            "src/latest_config.rs",
            "src/models/attestation.rs",
            "src/protest.rs",
        ] {
            assert!(!is_test_path(p), "{p}");
        }
    }

    #[test]
    fn cfg_test_module_is_marked_to_its_closing_brace() {
        let src = "\
fn real() { x.unwrap(); }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t() {
        let s = \"}{\";
        y.unwrap();
    }
}
fn after() { z.unwrap(); }";
        let set = rust_test_lines(&numbered(src), false);
        assert!(!set.contains(&1), "code before is not test code");
        for n in 2..=10 {
            assert!(set.contains(&n), "line {n} is inside the test module");
        }
        assert!(!set.contains(&11), "code after the module is not test code");
    }

    #[test]
    fn braces_in_strings_chars_and_comments_do_not_end_the_region() {
        let src = "\
#[cfg(test)]
mod tests {
    fn a() { let c = '}'; let s = \"}}}\"; } // }}}
    fn b() {}
}
fn after() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(set.contains(&4) && set.contains(&5));
        assert!(!set.contains(&6));
    }

    #[test]
    fn test_attribute_on_a_function_marks_only_that_function() {
        let src = "\
fn keep() {}
#[test]
fn only_this() {
    a.unwrap();
}
fn keep_too() { b.unwrap(); }";
        let set = rust_test_lines(&numbered(src), false);
        assert!((2..=5).all(|n| set.contains(&n)));
        assert!(!set.contains(&1) && !set.contains(&6));
    }

    #[test]
    fn test_attribute_on_a_statement_item_marks_one_line() {
        let src = "#[cfg(test)]\nuse helper::x;\nfn real() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(set.contains(&1) && set.contains(&2) && !set.contains(&3));
    }

    #[test]
    fn stacked_attributes_and_doc_comments_stay_in_the_region() {
        let src = "#[cfg(test)]\n#[allow(dead_code)]\n/// helper\nmod tests {\n}\nfn real() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!((1..=5).all(|n| set.contains(&n)) && !set.contains(&6));
    }

    #[test]
    fn a_gap_in_numbering_abandons_an_unfinished_region_but_keeps_what_was_seen() {
        // Diff view: the module is open at line 3 and the next shown line is 40.
        let lines = vec![
            (1, "#[cfg(test)]"),
            (2, "mod tests {"),
            (3, "    fn a() {"),
            (40, "    x.unwrap();"),
        ];
        let set = rust_test_lines(&lines, true);
        assert!(set.contains(&1) && set.contains(&2) && set.contains(&3));
        assert!(!set.contains(&40), "cannot vouch for lines after a gap");
    }

    #[test]
    fn plain_code_has_no_test_lines() {
        let set = rust_test_lines(&numbered("fn main() {\n    println!(\"hi\");\n}"), false);
        assert!(set.is_empty());
    }

    #[test]
    fn test_module_hunk_headers() {
        assert!(hunk_header_is_test_module("mod tests {"));
        assert!(hunk_header_is_test_module("pub mod test {"));
        assert!(!hunk_header_is_test_module("mod testing_utils {"));
        assert!(!hunk_header_is_test_module("fn tests() {"));
        assert!(!hunk_header_is_test_module(""));
    }

    #[test]
    fn an_unclosed_region_in_a_whole_file_is_dropped_not_extended_to_eof() {
        // A hostile or unparseable `{` must not exempt the rest of the file.
        let src = "\
#[cfg(test)]
fn helper() {
    let weird = '\\u{7b}'; let open = r#\"{{{\"#; let more = \"{\"; // tricks
    {
}
fn production() { danger.unwrap(); }
fn more_production() { x.unwrap(); }";
        let lines = numbered(src);
        // Whole-file view: no region is vouched for at all.
        let whole = rust_test_lines(&lines, false);
        assert!(!whole.contains(&6) && !whole.contains(&7), "{whole:?}");
        // Partial view (a diff): the column-0 `fn production` inside the
        // still-open region is a disagreement, so nothing is vouched for.
        let partial = rust_test_lines(&lines, true);
        assert!(
            !partial.contains(&6) && !partial.contains(&7),
            "{partial:?}"
        );
    }

    #[test]
    fn single_line_raw_strings_do_not_confuse_brace_matching() {
        let src = "#[cfg(test)]\nmod tests {\n    fn a() { let s = r#\"}}}\"#; }\n}\nfn real() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(
            (1..=4).all(|n| set.contains(&n)) && !set.contains(&5),
            "{set:?}"
        );
    }

    #[test]
    fn a_block_comment_brace_cannot_extend_the_test_region() {
        // `/* { */` is a comment: the test fn ends on line 2, `production`
        // is NOT test code. (Counting the comment brace used to extend the
        // region over it.)
        let src = "\
#[cfg(test)]
fn t() { /* { */ }
fn production() { danger.unwrap(); }
/* } */";
        let set = rust_test_lines(&numbered(src), false);
        assert!(set.contains(&2));
        assert!(!set.contains(&3), "{set:?}");
    }

    #[test]
    fn a_new_top_level_item_inside_an_open_region_abandons_it() {
        // Multi-line raw string the matcher cannot see: the open brace inside
        // makes the region look unfinished, and a column-0 item follows. Vouch
        // for nothing rather than exempt `production`.
        let src = "\
#[cfg(test)]
fn t() {
    let s = r#\"
{
\"#;
}
fn production() { danger.unwrap(); }
/* } */";
        let set = rust_test_lines(&numbered(src), false);
        assert!(!set.contains(&7), "{set:?}");
        let partial = rust_test_lines(&numbered(src), true);
        assert!(!partial.contains(&7), "{partial:?}");
    }

    #[test]
    fn multi_line_block_comments_hide_their_braces() {
        let src =
            "#[cfg(test)]\nmod tests {\n    /* open {\n       { */\n    fn a() {}\n}\nfn real() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(
            (1..=6).all(|n| set.contains(&n)) && !set.contains(&7),
            "{set:?}"
        );
    }

    #[test]
    fn indented_modules_close_at_their_own_indentation() {
        let src = "mod outer {\n    #[cfg(test)]\n    mod tests {\n        fn a() {}\n    }\n    fn prod() {}\n}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(
            (2..=5).all(|n| set.contains(&n)) && !set.contains(&6),
            "{set:?}"
        );
    }

    #[test]
    fn column_zero_fixture_text_inside_a_test_module_does_not_abandon_it() {
        // A TOML fixture in a raw string, flush left: not Rust items.
        let src = "\
#[cfg(test)]
mod tests {
    fn t() {
        let toml = r#\"
[embed]
api_key = \"sk-test\"
\"#;
    }
}
fn production() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!((1..=9).all(|n| set.contains(&n)), "{set:?}");
        assert!(!set.contains(&10));
    }

    #[test]
    fn item_start_detection() {
        for yes in [
            "fn a() {",
            "pub fn a() {",
            "pub(crate) async fn a() {",
            "unsafe impl X for Y {",
            "impl<T> Z for T {",
            "struct S;",
            "use std::fs;",
            "#[derive(Debug)]",
            "thread_local! {",
            "macro_rules! m {",
            "extern \"C\" fn f() {",
        ] {
            assert!(looks_like_item_start(yes), "{yes}");
        }
        for no in [
            "api_key = \"x\"",
            "[embed]",
            "{",
            "\"text\"",
            "foo.bar();",
            "}",
        ] {
            assert!(!looks_like_item_start(no), "{no}");
        }
    }

    #[test]
    fn an_attribute_with_its_item_on_the_same_line_does_not_exempt_the_next_line() {
        let src = "\
#[cfg(test)] fn helper() {}
fn production() { danger.unwrap(); }
#[cfg(test)]
#[allow(dead_code)] fn other() {}
fn production2() { danger.unwrap(); }";
        let set = rust_test_lines(&numbered(src), false);
        assert!(
            set.contains(&1) && set.contains(&3) && set.contains(&4),
            "{set:?}"
        );
        assert!(!set.contains(&2) && !set.contains(&5), "{set:?}");
    }

    #[test]
    fn a_same_line_item_with_a_body_that_spans_lines_is_one_region() {
        let src = "#[cfg(test)] mod tests {\n    fn a() {}\n}\nfn production() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(
            (1..=3).all(|n| set.contains(&n)) && !set.contains(&4),
            "{set:?}"
        );
    }

    #[test]
    fn an_attribute_inside_a_block_comment_is_not_an_attribute() {
        let src = "/*\n#[cfg(test)]\n*/\nfn production() { danger.unwrap(); }\n/* #[test] */\nfn also_production() {}";
        let set = rust_test_lines(&numbered(src), false);
        assert!(set.is_empty(), "{set:?}");
    }

    #[test]
    fn block_comment_state_does_not_go_stale_between_regions() {
        // An earlier multi-line comment must not leak into brace counting of
        // the next test module, and a closed region must not leave state behind.
        let src = "\
/* a comment
   { with a brace
*/
#[cfg(test)]
mod tests {
    fn a() {}
}
fn production() { danger.unwrap(); }
#[cfg(test)]
mod more {
}
";
        let set = rust_test_lines(&numbered(src), false);
        assert!((4..=7).all(|n| set.contains(&n)), "{set:?}");
        assert!(!set.contains(&8), "{set:?}");
        assert!((9..=11).all(|n| set.contains(&n)), "{set:?}");
    }
}
