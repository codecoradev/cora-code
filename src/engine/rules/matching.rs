/// Matching engine for rules against diff lines.
use tracing::debug;

use super::types::CustomRule;

/// Check if a rule applies to the given file language.
pub fn matches_language(rule: &CustomRule, file_lang: &str) -> bool {
    rule.languages
        .iter()
        .any(|lang| match normalize_language(lang) {
            Some(n) => {
                // A scan sees the raw extension (`tsx`), a diff the shorthand (`ts`).
                n == "all"
                    || n == file_lang
                    || normalize_language(file_lang).as_deref() == Some(n.as_str())
            }
            // Unknown value: keep the old literal comparison (config validation reports it).
            None => lang.to_lowercase() == file_lang,
        })
}

/// Map a configured language (extension or name) to the shorthand used for
/// files (`"rust"` and `"rs"` -> `"rs"`). `"all"` / `"*"` -> `"all"`.
/// `None` when it names no known language.
pub fn normalize_language(lang: &str) -> Option<String> {
    let l = lang.trim().trim_start_matches('.').to_lowercase();
    if l == "all" || l == "*" {
        return Some("all".to_string());
    }
    let by_name = match l.as_str() {
        "rust" => "rs",
        "python" => "py",
        "typescript" => "ts",
        "javascript" | "node" => "js",
        "golang" => "go",
        "kotlin" => "kt",
        "ruby" => "rb",
        "shell" | "bash" => "sh",
        "markdown" => "md",
        "csharp" | "c#" => "cs",
        "c++" => "cpp",
        "elixir" => "ex",
        _ => "",
    };
    if !by_name.is_empty() {
        return Some(by_name.to_string());
    }
    let detected = crate::engine::diff_parser::detect_language(&format!("x.{l}"));
    if detected == "unknown" {
        // Languages detect_language does not know but that are real extensions.
        return matches!(l.as_str(), "cs" | "vue" | "tf" | "xml" | "ini" | "txt").then_some(l);
    }
    Some(detected.to_string())
}

/// Check if a file path should be excluded from a rule based on glob patterns.
pub fn matches_exclude(rule: &CustomRule, file_path: &str) -> bool {
    // Also check built-in default exclude paths
    let all_excludes: Vec<&str> = rule
        .exclude
        .iter()
        .map(String::as_str)
        .chain(DEFAULT_EXCLUDE_PATHS.iter().copied())
        .collect();

    for pattern in &all_excludes {
        // Simple glob matching: treat * as any chars, support directory prefixes
        if glob_matches(pattern, file_path) {
            return true;
        }
    }

    false
}

/// Check if a rule's regex pattern matches a line of code.
pub fn match_rule_against_line(rule: &CustomRule, line: &str) -> bool {
    match &rule.compiled_pattern {
        Some(re) => re.is_match(line),
        None => {
            debug!(rule_id = %rule.id, "regex not compiled, skipping");
            false
        }
    }
}

/// Simple glob matching: supports `*` wildcard and directory prefix checks.
fn glob_matches(pattern: &str, path: &str) -> bool {
    // Handle "*_test.*" style patterns
    if pattern.starts_with('*') && pattern.contains('.') {
        // e.g., "*_test.*" — match files ending with "_test.something"
        let suffix = &pattern[1..]; // "_test.*"
        if let Some(dot_idx) = suffix.find('.') {
            let name_part = &suffix[..dot_idx]; // "_test"
            let ext_part = &suffix[dot_idx..]; // ".*"
            let file_name = path.rsplit('/').next().unwrap_or(path);
            if file_name.ends_with(name_part) {
                let remaining = &file_name[file_name.len() - name_part.len()..];
                if remaining == name_part {
                    // Check extension
                    if ext_part == ".*" {
                        return true;
                    }
                    if let Some(file_ext) = file_name.rsplit_once(name_part).map(|(_, ext)| ext) {
                        if file_ext.starts_with('.')
                            && ext_part
                                .strip_prefix('.')
                                .is_some_and(|e| file_ext.ends_with(e))
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // Handle "dir/" prefix patterns (directory-based exclusion). Match only at
    // path-segment boundaries so a `src/` pattern doesn't also catch `mysrc/`
    // or `docs/src-guide/` (#66).
    if pattern.ends_with('/') {
        return path.starts_with(pattern) || dir_segment_match(pattern, path);
    }

    // Handle "**" glob in patterns like "tests/**"
    if pattern.contains("**") {
        let prefix = pattern.split("**").next().unwrap_or("");
        return path.starts_with(prefix);
    }

    // Handle simple substring patterns
    if pattern.starts_with('*') && pattern.ends_with('*') {
        let inner = &pattern[1..pattern.len() - 1];
        return path.contains(inner);
    }

    // Exact or prefix match
    path == pattern || path.starts_with(&format!("{pattern}/"))
}

/// True if a directory `pattern` (with trailing `/`) matches a full path
/// segment somewhere inside `path` — e.g. `src/` matches `foo/src/bar.rs`
/// but not `mysrc/bar.rs` (#66).
fn dir_segment_match(pattern: &str, path: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = path[search_from..].find(pattern) {
        let abs = search_from + rel;
        if abs == 0 || path.as_bytes().get(abs - 1) == Some(&b'/') {
            return true;
        }
        search_from = abs + 1;
    }
    false
}

/// Default paths to exclude from rule matching (test directories, fixtures, etc.).
const DEFAULT_EXCLUDE_PATHS: &[&str] = &[
    "tests/",
    "test/",
    "__tests__/",
    "spec/",
    "fixtures/",
    "examples/",
    "rules/",
    // File patterns handled in glob_matches: *_test.*, *_spec.*
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;

    fn make_rule(pattern: &str, languages: &[&str], exclude: &[&str]) -> CustomRule {
        let mut rule = CustomRule {
            id: "test-rule".to_string(),
            pattern: pattern.to_string(),
            severity: Severity::Minor,
            message: "test".to_string(),
            languages: languages.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            compiled_pattern: None,
        };
        rule.ensure_compiled();
        rule
    }

    #[test]
    fn matches_language_all() {
        let rule = make_rule("test", &["all"], &[]);
        assert!(matches_language(&rule, "rs"));
        assert!(matches_language(&rule, "py"));
        assert!(matches_language(&rule, "js"));
    }

    #[test]
    fn matches_language_specific() {
        let rule = make_rule("test", &["rs"], &[]);
        assert!(matches_language(&rule, "rs"));
        assert!(!matches_language(&rule, "py"));
    }

    #[test]
    fn matches_language_case_insensitive() {
        let rule = make_rule("test", &["RS"], &[]);
        assert!(matches_language(&rule, "rs"));
    }

    #[test]
    fn matches_exclude_test_dir() {
        let rule = make_rule("test", &["all"], &[]);
        assert!(matches_exclude(&rule, "tests/main.rs"));
        assert!(matches_exclude(&rule, "test/helper.py"));
    }

    #[test]
    fn not_excluded_src_dir() {
        let rule = make_rule("test", &["all"], &[]);
        assert!(!matches_exclude(&rule, "src/main.rs"));
    }

    #[test]
    fn glob_matches_dir_at_segment_boundary_only() {
        // #66: a `src/` exclude must not match `mysrc/` or `docs/src-guide/`.
        assert!(glob_matches("src/", "src/main.rs"));
        assert!(glob_matches("src/", "apps/web/src/main.rs"));
        assert!(!glob_matches("src/", "mysrc/main.rs"));
        assert!(!glob_matches("src/", "docs/src-guide.md"));
    }

    #[test]
    fn matches_exclude_custom() {
        let rule = make_rule("test", &["all"], &["vendor/"]);
        assert!(matches_exclude(&rule, "vendor/lib.rs"));
    }

    #[test]
    fn match_rule_against_line_simple() {
        let rule = make_rule(r"println!", &["all"], &[]);
        assert!(match_rule_against_line(&rule, "println!(\"hello\")"));
        assert!(!match_rule_against_line(&rule, "let x = 1;"));
    }

    #[test]
    fn match_rule_invalid_regex_returns_false() {
        let rule = make_rule(r"(?P<invalid", &["all"], &[]);
        assert!(!match_rule_against_line(&rule, "anything"));
    }

    #[test]
    fn glob_matches_star_test_star() {
        assert!(glob_matches("*_test.*", "src/helper_test.rs"));
        assert!(glob_matches("*_test.*", "foo_test.js"));
    }

    #[test]
    fn glob_matches_dir_prefix() {
        assert!(glob_matches("tests/**", "tests/integration/main.rs"));
        assert!(glob_matches("spec/", "spec/unit/test.rs"));
    }

    #[test]
    fn glob_matches_custom_exclude() {
        assert!(glob_matches("vendor/**", "vendor/lib.rs"));
    }
}
