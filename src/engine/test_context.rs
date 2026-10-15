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
fn brace_delta(line: &str) -> i32 {
    let mut delta = 0;
    let mut chars = line.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
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
            '"' => in_str = true,
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
pub fn rust_test_lines(lines: &[(u32, &str)]) -> HashSet<u32> {
    let mut out = HashSet::new();
    // (lines of the region so far, brace depth, seen the opening brace)
    let mut region: Option<(Vec<u32>, i32, bool)> = None;
    let mut pending: Vec<u32> = Vec::new();
    let mut prev: Option<u32> = None;

    for &(n, text) in lines {
        if prev.is_some_and(|p| n != p + 1) {
            // Discontinuous: drop what we cannot finish (the lines already
            // collected stay marked: they were inside the region).
            if let Some((seen, _, _)) = region.take() {
                out.extend(seen);
            }
            pending.clear();
        }
        prev = Some(n);
        let trimmed = text.trim();

        if let Some((seen, depth, opened)) = region.as_mut() {
            seen.push(n);
            *depth += brace_delta(text);
            if text.contains('{') {
                *opened = true;
            }
            let done = (*opened && *depth <= 0) || (!*opened && trimmed.ends_with(';'));
            if done {
                let (seen, _, _) = region.take().expect("region present");
                out.extend(seen);
            }
            continue;
        }

        if is_test_attribute(trimmed) {
            pending.push(n);
            continue;
        }
        if !pending.is_empty() {
            // Other attributes and blank lines between the test attribute and
            // the item keep the region pending.
            if trimmed.starts_with("#[") || trimmed.is_empty() || trimmed.starts_with("///") {
                pending.push(n);
                continue;
            }
            // The decorated item starts here.
            let mut seen = std::mem::take(&mut pending);
            seen.push(n);
            let depth = brace_delta(text);
            let opened = text.contains('{');
            if (opened && depth <= 0) || (!opened && trimmed.ends_with(';')) {
                out.extend(seen);
            } else {
                region = Some((seen, depth, opened));
            }
        }
    }
    if let Some((seen, _, _)) = region {
        out.extend(seen);
    }
    out
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
        let set = rust_test_lines(&numbered(src));
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
        let set = rust_test_lines(&numbered(src));
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
        let set = rust_test_lines(&numbered(src));
        assert!((2..=5).all(|n| set.contains(&n)));
        assert!(!set.contains(&1) && !set.contains(&6));
    }

    #[test]
    fn test_attribute_on_a_statement_item_marks_one_line() {
        let src = "#[cfg(test)]\nuse helper::x;\nfn real() {}";
        let set = rust_test_lines(&numbered(src));
        assert!(set.contains(&1) && set.contains(&2) && !set.contains(&3));
    }

    #[test]
    fn stacked_attributes_and_doc_comments_stay_in_the_region() {
        let src = "#[cfg(test)]\n#[allow(dead_code)]\n/// helper\nmod tests {\n}\nfn real() {}";
        let set = rust_test_lines(&numbered(src));
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
        let set = rust_test_lines(&lines);
        assert!(set.contains(&1) && set.contains(&2) && set.contains(&3));
        assert!(!set.contains(&40), "cannot vouch for lines after a gap");
    }

    #[test]
    fn plain_code_has_no_test_lines() {
        let set = rust_test_lines(&numbered("fn main() {\n    println!(\"hi\");\n}"));
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
}
