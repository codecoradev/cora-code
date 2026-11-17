//! Language-specific analyzers — enriched prompts for LLM to understand language idioms.
//!
//! Not a static analyzer. Injects language-specific context into the review prompt
//! so the LLM understands language-specific patterns and anti-patterns.

use crate::engine::diff_parser;

/// Build language-specific context from already-parsed diff chunks.
///
/// Uses pre-parsed chunks to avoid redundant parsing.
pub fn build_language_context_from_chunks(chunks: &[diff_parser::FileChunk]) -> String {
    let languages = detect_languages(chunks);

    if languages.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    parts.push("LANGUAGE-SPECIFIC REVIEW GUIDANCE:".to_string());

    for lang in &languages {
        if let Some(rules) = get_language_rules(lang) {
            parts.push(rules.to_string());
        }
    }

    parts.join("\n\n")
}

/// Build language-specific context string from raw diff text.
#[cfg(test)]
pub fn build_language_context(diff_text: &str) -> String {
    let chunks = diff_parser::parse_diff(diff_text);
    let languages = detect_languages(&chunks);

    if languages.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    parts.push("LANGUAGE-SPECIFIC REVIEW GUIDANCE:".to_string());

    for lang in &languages {
        if let Some(rules) = get_language_rules(lang) {
            parts.push(rules.to_string());
        }
    }

    parts.join("\n\n")
}

/// Detect unique languages present in diff chunks.
fn detect_languages(chunks: &[diff_parser::FileChunk]) -> Vec<String> {
    let mut langs = std::collections::HashSet::new();
    for chunk in chunks {
        let path = chunk
            .new_path
            .as_deref()
            .or(chunk.old_path.as_deref())
            .unwrap_or("");
        let lang = diff_parser::detect_language(path);
        if lang != "unknown" && lang != "image" && lang != "document" && lang != "lock" {
            langs.insert(lang.to_string());
        }
    }
    let mut result: Vec<String> = langs.into_iter().collect();
    result.sort();
    result
}

/// Get language-specific review rules.
fn get_language_rules(lang: &str) -> Option<&'static str> {
    match lang {
        "dart" => Some(DART_RULES),
        "svelte" => Some(SVELTE_RULES),
        "ts" | "js" => Some(TYPESCRIPT_RULES),
        "go" => Some(GO_RULES),
        "rs" => Some(RUST_RULES),
        "py" => Some(PYTHON_RULES),
        _ => None,
    }
}

// ─── Language-specific rules ───

const DART_RULES: &str = r#"## Dart/Flutter Review Focus
- **Widget lifecycle**: Check for `initState`/`dispose` mismatches, missing `super` calls
- **State management**: Flag direct `setState` in large widgets (prefer BLoC/Riverpod)
- **Async patterns**: Check for missing `await`, uncaught `Future` errors
- **Null safety**: Flag `!` operator misuse, prefer null-aware operators (`?.`, `??`)
- **Memory leaks**: Check for uncancelled timers, unclosed streams, missing `dispose`
- **Performance**: Flag `const` constructors that could be used, unnecessary rebuilds"#;

const SVELTE_RULES: &str = r#"## Svelte/SvelteKit Review Focus
- **Reactive declarations**: Check `$:` label correctness, dependency tracking issues
- **Store patterns**: Flag direct store mutation (use `$store` for reads, `store.set()`/`store.update()` for writes)
- **SSR considerations**: Check for browser-only APIs (`window`, `document`) in load functions
- **Lifecycle**: Check `onMount`/`onDestroy` cleanup, missing `onDestroy` for subscriptions
- **Type safety**: Flag `any` types in `.svelte` files, prefer typed props
- **Performance**: Flag unnecessary reactive statements, prefer `{#each}` with `key`"#;

const TYPESCRIPT_RULES: &str = r#"## TypeScript Review Focus
- **Type safety**: Flag `any` types, prefer generics and union types
- **Null handling**: Check for missing null checks, prefer optional chaining (`?.`)
- **Error handling**: Flag uncaught promises, prefer `try/catch` with proper typing
- **Async patterns**: Check for missing `await`, floating promises
- **Performance**: Flag unnecessary type assertions, prefer `const` assertions
- **Best practices**: Prefer interfaces over type aliases for objects, use `unknown` over `any`"#;

const GO_RULES: &str = r#"## Go Review Focus
- **Error handling**: Flag ignored errors (`_ = riskyCall()`), prefer explicit error check
- **Goroutine safety**: Check for race conditions, missing mutex, channel leaks
- **Resource cleanup**: Flag missing `defer Close()`, unclosed response bodies
- **Context propagation**: Check for missing `context.Context` in function signatures
- **Slice/map safety**: Flag concurrent access without synchronization
- **Error wrapping**: Prefer `fmt.Errorf("...: %w", err)` for error chains"#;

const RUST_RULES: &str = r#"## Rust Review Focus
- **Unsafe usage**: Flag `unsafe` blocks, prefer safe alternatives
- **Error handling**: Flag `unwrap()`/`expect()` in non-test code, prefer `?` operator
- **Lifetime issues**: Check for unnecessary lifetime annotations, lifetime elision
- **Ownership**: Flag unnecessary `clone()`, prefer borrowing
- **Concurrency**: Check for `Send`/`Sync` violations, deadlock patterns
- **Performance**: Flag `String` where `&str` suffices, unnecessary allocations"#;

const PYTHON_RULES: &str = r#"## Python Review Focus
- **Type hints**: Flag missing return type hints, `Any` overuse
- **Exception handling**: Flag bare `except:`, prefer specific exception types
- **Resource management**: Flag missing `with` statement for file/connection handling
- **Security**: Flag `pickle.loads` on untrusted data, `subprocess` with shell=True
- **Async patterns**: Check for missing `await`, blocking calls in async functions
- **Best practices**: Prefer `pathlib` over `os.path`, f-strings over `.format()`"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_languages_from_diff() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,1 +1,1 @@
-fn old() {}
+fn new() {}
";
        let langs = detect_languages(&diff_parser::parse_diff(diff));
        assert!(langs.contains(&"rs".to_string()));
    }

    #[test]
    fn detect_multiple_languages() {
        let diff = "\
diff --git a/main.go b/main.go
--- a/main.go
+++ b/main.go
@@ -1,1 +1,1 @@
-old
+new
diff --git a/app.py b/app.py
--- a/app.py
+++ b/app.py
@@ -1,1 +1,1 @@
-old
+new
";
        let langs = detect_languages(&diff_parser::parse_diff(diff));
        assert!(langs.contains(&"go".to_string()));
        assert!(langs.contains(&"py".to_string()));
        assert_eq!(langs.len(), 2);
    }

    #[test]
    fn build_context_not_empty_for_rs() {
        let diff = "\
diff --git a/main.rs b/main.rs
--- a/main.rs
+++ b/main.rs
@@ -1,1 +1,1 @@
-old
+new
";
        let ctx = build_language_context(diff);
        assert!(ctx.contains("Rust"));
        assert!(ctx.contains("unsafe"));
    }

    #[test]
    fn build_context_for_dart() {
        let diff = "\
diff --git a/lib/main.dart b/lib/main.dart
--- a/lib/main.dart
+++ b/lib/main.dart
@@ -1,1 +1,1 @@
-old
+new
";
        let ctx = build_language_context(diff);
        assert!(ctx.contains("Flutter"));
    }

    #[test]
    fn build_context_for_svelte() {
        let diff = "\
diff --git a/src/App.svelte b/src/App.svelte
--- a/src/App.svelte
+++ b/src/App.svelte
@@ -1,1 +1,1 @@
-old
+new
";
        let ctx = build_language_context(diff);
        assert!(ctx.contains("Svelte"));
    }

    #[test]
    fn build_context_for_go() {
        let diff = "\
diff --git a/main.go b/main.go
--- a/main.go
+++ b/main.go
@@ -1,1 +1,1 @@
-old
+new
";
        let ctx = build_language_context(diff);
        assert!(ctx.contains("Go"));
        assert!(ctx.contains("Error handling"));
    }

    #[test]
    fn empty_diff_no_context() {
        let ctx = build_language_context("");
        assert!(ctx.is_empty());
    }

    #[test]
    fn all_rules_available() {
        assert!(get_language_rules("dart").is_some());
        assert!(get_language_rules("svelte").is_some());
        assert!(get_language_rules("ts").is_some());
        assert!(get_language_rules("js").is_some());
        assert!(get_language_rules("go").is_some());
        assert!(get_language_rules("rs").is_some());
        assert!(get_language_rules("py").is_some());
        assert!(get_language_rules("unknown").is_none());
    }
}
