//! Symbol extraction from source code lines.
//!
//! Uses language-aware regex patterns to extract function calls, type
//! references, and import statements from changed diff lines. No full
//! AST parsing — regex is sufficient for the deterministic context chain
//! and avoids pulling in heavy dependencies like `syn`.

use std::collections::HashSet;

use regex::Regex;
use std::sync::LazyLock;
use tracing::debug;

use super::types::{ExtractedSymbol, SymbolKind};

// --- Regex patterns by language ---

/// Rust: `use crate::foo::bar` or `use super::baz` or `mod foo`
static RE_RUST_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\buse\s+(crate|super|self)::([\w:]+)").unwrap());

/// Rust: module declaration `mod foo;` or `mod foo {` (#73).
static RE_RUST_MOD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bmod\s+(\w+)").unwrap());

/// Rust: function call — identifier followed by `(`, possibly preceded by `::` or `.`
static RE_RUST_FN_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\w+::)?\w+(?:::\w+)*\s*\(").unwrap());

/// Rust: type-like references — `Type`, `Option<T>`, `Vec<String>`, after `: `, `<`, or `as`
static RE_RUST_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:(?::\s)|(?:<)|(?:as\s)|(?:->\s))([A-Z]\w+)").unwrap());

/// Python: `import foo` or `from foo import bar`
static RE_PY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:from|import)\s+([\w.]+)").unwrap());

/// Python: function call — `foo(` or `obj.method(`
static RE_PY_FN_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());

/// Python: type annotation — `: SomeType` or `-> ReturnType`
static RE_PY_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?::\s*[A-Z]\w*|->\s*[A-Z]\w*)").unwrap());

/// JS/TS: `import ... from '...'`, `require('...')`, or dynamic `import('...')`
static RE_JS_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:import\s+.*?\s+from\s*['"]([^'"]+)['"]|require\s*\(\s*['"]([^'"]+)['"]|import\s*\(\s*['"]([^'"]+)['"])"#).unwrap()
});

/// JS/TS: function call — `foo(` or `obj.method(`
static RE_JS_FN_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());

/// Go: `import "foo"` or `import foo "bar"`
static RE_GO_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bimport\s+[\w.]*\s*["']([^"']+)["']"#).unwrap());

/// Go: function call — `foo(` or `pkg.Func(`
static RE_GO_FN_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z]\w*\.\w+|\w+)\s*\(").unwrap());

/// Go: type reference — `SomeType{` or `var x SomeType` or `: SomeType`
static RE_GO_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\b([A-Z]\w+)\s*\{|:\s*([A-Z]\w+))").unwrap());

/// Java: `import foo.bar.*` (also `import static foo.Bar.baz`)
static RE_JAVA_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bimport\s+(?:static\s+)?([\w.*]+)").unwrap());

/// Java: method call — `foo(` or `obj.method(`
static RE_JAVA_FN_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());

/// Java: type reference — `SomeType ` or `<SomeType>`
static RE_JAVA_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Z]\w+)\s*[<{( ]").unwrap());

/// Maximum number of symbols to extract per file to prevent runaway extraction.
const MAX_SYMBOLS_PER_FILE: usize = 50;

// ─── Definition regexes (functions/types *declared* in changed lines) ───
// Used for inbound caller (blast-radius) resolution.

static RE_DEF_RUST_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)").unwrap());
static RE_DEF_RUST_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:pub\s+)?(?:struct|enum|trait|type)\s+(\w+)").unwrap());
static RE_DEF_PY_FN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bdef\s+(\w+)").unwrap());
static RE_DEF_PY_TYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bclass\s+(\w+)").unwrap());
static RE_DEF_JS_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bfunction\s+\*?\s*(\w+)").unwrap());
static RE_DEF_JS_TYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bclass\s+(\w+)").unwrap());
static RE_DEF_GO_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bfunc\s+(?:\([^)]*\)\s*)?(\w+)\s*\(").unwrap());
static RE_DEF_GO_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\btype\s+(\w+)\s+(?:struct|interface)").unwrap());
static RE_DEF_JAVA_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:class|interface|enum)\s+(\w+)").unwrap());

/// Extract symbols from a single line of code for a given language.
pub fn extract_symbols_from_line(line: &str, language: &str) -> Vec<SymbolKind> {
    let mut symbols = Vec::new();
    let mut seen = HashSet::new();

    let add_unique = |syms: &mut Vec<SymbolKind>, seen: &mut HashSet<String>, s: SymbolKind| {
        let key = format!("{s}");
        if seen.insert(key) {
            syms.push(s);
        }
    };

    match language {
        "rs" => {
            // Imports (highest value for resolution)
            for cap in RE_RUST_IMPORT.captures_iter(line) {
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::Import(cap.get(2).unwrap().as_str().to_string()),
                );
            }
            // Module declarations (#73)
            for cap in RE_RUST_MOD.captures_iter(line) {
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::Import(cap.get(1).unwrap().as_str().to_string()),
                );
            }
            // Function calls
            for cap in RE_RUST_FN_CALL.captures_iter(line) {
                let raw = cap.get(0).unwrap().as_str().trim_end_matches('(').trim();
                // Skip macros like `println!`, `vec!`, `format!`, `debug!`, etc.
                if raw.ends_with('!') {
                    continue;
                }
                // Skip common keywords that look like function calls
                if matches!(
                    raw,
                    "if" | "while" | "for" | "loop" | "match" | "return" | "break"
                ) {
                    continue;
                }
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::FunctionCall(raw.to_string()),
                );
            }
            // Type references
            for cap in RE_RUST_TYPE.captures_iter(line) {
                if let Some(type_name) = cap.get(1) {
                    let name = type_name.as_str().to_string();
                    add_unique(&mut symbols, &mut seen, SymbolKind::TypeRef(name));
                }
            }
        }
        "py" | "pyi" => {
            for cap in RE_PY_IMPORT.captures_iter(line) {
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::Import(cap.get(1).unwrap().as_str().to_string()),
                );
            }
            for cap in RE_PY_FN_CALL.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                if matches!(
                    name.as_str(),
                    "if" | "while" | "for" | "print" | "range" | "len"
                ) {
                    continue;
                }
                add_unique(&mut symbols, &mut seen, SymbolKind::FunctionCall(name));
            }
            for cap in RE_PY_TYPE.captures_iter(line) {
                let full = cap.get(0).unwrap().as_str();
                // Extract the type name after `:` or `->`
                let name = full.trim_start_matches(':').trim_start_matches('-').trim();
                if !name.is_empty() && name.starts_with(char::is_uppercase) {
                    add_unique(
                        &mut symbols,
                        &mut seen,
                        SymbolKind::TypeRef(name.to_string()),
                    );
                }
            }
        }
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            for cap in RE_JS_IMPORT.captures_iter(line) {
                // The regex has 3 capture groups (one per alternative). Take the first non-None.
                let path = cap
                    .get(1)
                    .or_else(|| cap.get(2))
                    .or_else(|| cap.get(3))
                    .unwrap()
                    .as_str()
                    .to_string();
                add_unique(&mut symbols, &mut seen, SymbolKind::Import(path));
            }
            for cap in RE_JS_FN_CALL.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                if matches!(name.as_str(), "if" | "while" | "for" | "require" | "import") {
                    continue;
                }
                add_unique(&mut symbols, &mut seen, SymbolKind::FunctionCall(name));
            }
        }
        "go" => {
            for cap in RE_GO_IMPORT.captures_iter(line) {
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::Import(cap.get(1).unwrap().as_str().to_string()),
                );
            }
            for cap in RE_GO_FN_CALL.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                if matches!(name.as_str(), "if" | "for" | "range" | "defer" | "go") {
                    continue;
                }
                add_unique(&mut symbols, &mut seen, SymbolKind::FunctionCall(name));
            }
            for cap in RE_GO_TYPE.captures_iter(line) {
                if let Some(type_name) = cap.get(1).or_else(|| cap.get(2)) {
                    add_unique(
                        &mut symbols,
                        &mut seen,
                        SymbolKind::TypeRef(type_name.as_str().to_string()),
                    );
                }
            }
        }
        "java" | "kt" | "kts" | "dart" => {
            for cap in RE_JAVA_IMPORT.captures_iter(line) {
                add_unique(
                    &mut symbols,
                    &mut seen,
                    SymbolKind::Import(cap.get(1).unwrap().as_str().to_string()),
                );
            }
            for cap in RE_JAVA_FN_CALL.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                if matches!(
                    name.as_str(),
                    "if" | "while" | "for" | "switch" | "return" | "new" | "instanceof"
                ) {
                    continue;
                }
                add_unique(&mut symbols, &mut seen, SymbolKind::FunctionCall(name));
            }
            for cap in RE_JAVA_TYPE.captures_iter(line) {
                if let Some(type_name) = cap.get(1) {
                    let name = type_name.as_str().to_string();
                    // Skip common non-user types
                    if matches!(
                        name.as_str(),
                        "String" | "Integer" | "Boolean" | "List" | "Map"
                    ) {
                        continue;
                    }
                    add_unique(&mut symbols, &mut seen, SymbolKind::TypeRef(name));
                }
            }
        }
        _ => {
            // Fallback: basic function call detection for any language
            for cap in RE_PY_FN_CALL.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                if name.len() > 2 {
                    add_unique(&mut symbols, &mut seen, SymbolKind::FunctionCall(name));
                }
            }
        }
    }

    symbols
}

/// Extract all symbols from the added lines of parsed diff chunks.
///
/// Returns a list of `ExtractedSymbol` entries, one per symbol found.
/// Duplicates across lines are deduplicated (same kind + same name + same file).
pub fn extract_symbols_from_diff(
    chunks: &[crate::engine::diff_parser::FileChunk],
) -> Vec<ExtractedSymbol> {
    let mut all_symbols = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (symbol key, file)

    for file in chunks {
        if file.is_binary || file.is_deleted {
            continue;
        }

        let file_path = file
            .new_path
            .as_deref()
            .or(file.old_path.as_deref())
            .unwrap_or("unknown");

        let mut file_symbol_count = 0;

        for hunk in &file.chunks {
            for line in &hunk.lines {
                // Only extract from added lines
                if line.line_type != crate::engine::diff_parser::DiffLineType::Add {
                    continue;
                }

                // Early cutoff: stop processing lines for this file once cap is reached
                if file_symbol_count >= MAX_SYMBOLS_PER_FILE {
                    break;
                }

                let language = &file.language;
                let symbols = extract_symbols_from_line(&line.content, language);

                for sym in symbols {
                    let key = format!("{sym}");
                    if seen.insert((key, file_path.to_string())) {
                        all_symbols.push(ExtractedSymbol {
                            file: file_path.to_string(),
                            line: line.new_line_no.unwrap_or(0),
                            kind: sym,
                            raw: line.content.clone(),
                        });
                        file_symbol_count += 1;
                        if file_symbol_count >= MAX_SYMBOLS_PER_FILE {
                            break;
                        }
                    }
                }
                if file_symbol_count >= MAX_SYMBOLS_PER_FILE {
                    break;
                }
            }
            if file_symbol_count >= MAX_SYMBOLS_PER_FILE {
                break;
            }
        }

        debug!(
            file = file_path,
            symbols = file_symbol_count,
            "extracted symbols from file"
        );
    }

    debug!(
        total = all_symbols.len(),
        "total symbols extracted from diff"
    );
    all_symbols
}

/// Extract function/type *definitions* declared in the added lines of a diff.
/// These are the symbols whose **callers** (blast radius) we want to resolve.
///
/// Only added lines are scanned (a definition only matters if this PR
/// introduces or modifies it). Returns deduplicated `(name, kind, file)`.
pub fn extract_definitions_from_diff(
    chunks: &[crate::engine::diff_parser::FileChunk],
) -> Vec<crate::engine::context::types::DefinedSymbol> {
    use crate::engine::context::types::{DefinedSymbol, DefinitionKind};
    use crate::engine::diff_parser::DiffLineType;
    let mut out = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (name, file)

    for file in chunks {
        if file.is_binary || file.is_deleted {
            continue;
        }
        let path = file
            .new_path
            .as_deref()
            .or(file.old_path.as_deref())
            .unwrap_or("unknown");
        let lang = &file.language;

        for hunk in &file.chunks {
            for line in &hunk.lines {
                if line.line_type != DiffLineType::Add {
                    continue;
                }
                let content = &line.content;
                let (fn_re, type_re): (Option<&Regex>, Option<&Regex>) = match lang.as_str() {
                    "rs" => (Some(&RE_DEF_RUST_FN), Some(&RE_DEF_RUST_TYPE)),
                    "py" | "pyi" => (Some(&RE_DEF_PY_FN), Some(&RE_DEF_PY_TYPE)),
                    "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                        (Some(&RE_DEF_JS_FN), Some(&RE_DEF_JS_TYPE))
                    }
                    "go" => (Some(&RE_DEF_GO_FN), Some(&RE_DEF_GO_TYPE)),
                    "java" | "kt" | "kts" | "dart" => (None, Some(&RE_DEF_JAVA_TYPE)),
                    _ => (None, None),
                };

                let mut push = |name: &str, kind: DefinitionKind| {
                    if name.is_empty() {
                        return;
                    }
                    // Skip obvious noise / keywords that slip through.
                    if matches!(
                        name,
                        "if" | "for" | "while" | "match" | "new" | "return" | "test" | "main"
                    ) {
                        return;
                    }
                    if seen.insert((name.to_string(), path.to_string())) {
                        out.push(DefinedSymbol {
                            name: name.to_string(),
                            kind,
                            file: path.to_string(),
                        });
                    }
                };

                if let Some(re) = fn_re {
                    for cap in re.captures_iter(content) {
                        push(&cap[1], DefinitionKind::Function);
                    }
                }
                if let Some(re) = type_re {
                    for cap in re.captures_iter(content) {
                        push(&cap[1], DefinitionKind::Type);
                    }
                }
            }
        }
    }

    debug!(definitions = out.len(), "extracted definitions from diff");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::diff_parser::{DiffHunk, DiffLine, DiffLineType, FileChunk, parse_diff};

    fn make_file_chunk(path: &str, lang: &str, added_lines: &[(&str, u32)]) -> FileChunk {
        FileChunk {
            old_path: Some(path.to_string()),
            new_path: Some(path.to_string()),
            language: lang.to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 0,
                new_start: 1,
                new_count: added_lines.len() as u32,
                header: String::new(),
                lines: added_lines
                    .iter()
                    .map(|(text, line_no)| DiffLine {
                        line_type: DiffLineType::Add,
                        content: text.to_string(),
                        old_line_no: None,
                        new_line_no: Some(*line_no),
                    })
                    .collect(),
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }
    }

    // --- Rust extraction ---

    #[test]
    fn extract_definitions_rust_fn_and_type() {
        let chunks = vec![make_file_chunk(
            "src/lib.rs",
            "rs",
            &[
                ("pub fn validate_token(token: &str) -> bool {", 1),
                ("pub struct CryptoConfig { seed: u64 }", 2),
            ],
        )];
        let defs = extract_definitions_from_diff(&chunks);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"validate_token"),
            "should detect fn definition"
        );
        assert!(
            names.contains(&"CryptoConfig"),
            "should detect struct definition"
        );
    }

    #[test]
    fn extract_definitions_python() {
        let chunks = vec![make_file_chunk(
            "app/auth.py",
            "py",
            &[("def login(user):", 1), ("class User:", 2)],
        )];
        let defs = extract_definitions_from_diff(&chunks);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"login"));
        assert!(names.contains(&"User"));
    }

    #[test]
    fn extract_definitions_go() {
        let chunks = vec![make_file_chunk(
            "main.go",
            "go",
            &[("func Parse(input string) error {", 1)],
        )];
        let defs = extract_definitions_from_diff(&chunks);
        assert!(defs.iter().any(|d| d.name == "Parse"));
    }

    #[test]
    fn extract_definitions_dedups_within_file() {
        let chunks = vec![make_file_chunk(
            "src/lib.rs",
            "rs",
            &[("fn helper() {}", 1), ("fn helper() {}", 2)],
        )];
        let defs = extract_definitions_from_diff(&chunks);
        assert_eq!(defs.iter().filter(|d| d.name == "helper").count(), 1);
    }

    #[test]
    fn extract_rust_function_calls() {
        let chunks = vec![make_file_chunk(
            "src/main.rs",
            "rs",
            &[("let result = validate_token(tok);", 5)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let fn_calls: Vec<_> = symbols
            .iter()
            .filter(
                |s| matches!(&s.kind, SymbolKind::FunctionCall(name) if name == "validate_token"),
            )
            .collect();
        assert!(
            !fn_calls.is_empty(),
            "should extract validate_token function call from Rust"
        );
    }

    #[test]
    fn extract_rust_imports() {
        let chunks = vec![make_file_chunk(
            "src/engine/mod.rs",
            "rs",
            &[("use crate::engine::scanner;", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p == "engine::scanner"))
            .collect();
        assert!(
            !imports.is_empty(),
            "should extract use crate::engine::scanner"
        );
    }

    #[test]
    fn extract_rust_module_declarations() {
        // #73: `mod foo;` should be extracted as an import-like dependency.
        let chunks = vec![make_file_chunk(
            "src/main.rs",
            "rs",
            &[("mod engine;", 1), ("pub mod config;", 2)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let mods: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p == "engine" || p == "config"))
            .collect();
        assert_eq!(mods.len(), 2, "should extract both mod declarations");
    }

    #[test]
    fn extract_java_wildcard_import() {
        // #72: `import foo.bar.*` should keep the wildcard, not truncate to `foo.bar`.
        let chunks = vec![make_file_chunk(
            "src/App.java",
            "java",
            &[("import com.example.*;", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p.contains("*")))
            .collect();
        assert!(
            !imports.is_empty(),
            "java wildcard import should keep the '*'"
        );
    }

    #[test]
    fn extract_rust_type_refs() {
        let chunks = vec![make_file_chunk(
            "src/config.rs",
            "rs",
            &[("let config: CryptoConfig = load();", 10)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let types: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::TypeRef(t) if t == "CryptoConfig"))
            .collect();
        assert!(
            !types.is_empty(),
            "should extract CryptoConfig type reference"
        );
    }

    #[test]
    fn rust_skips_macros() {
        let chunks = vec![make_file_chunk(
            "src/main.rs",
            "rs",
            &[
                ("println!(\"hello\");", 1),
                ("debug!(\"value: {}\", x);", 2),
            ],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let fn_calls: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::FunctionCall(_)))
            .collect();
        assert!(
            fn_calls.is_empty(),
            "should skip Rust macros (println!, debug!, etc.)"
        );
    }

    #[test]
    fn rust_skips_keywords() {
        let chunks = vec![make_file_chunk(
            "src/main.rs",
            "rs",
            &[("if condition { return; }", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let fn_calls: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::FunctionCall(name) if name == "if" || name == "return"))
            .collect();
        assert!(fn_calls.is_empty(), "should skip Rust keyword-like calls");
    }

    // --- Python extraction ---

    #[test]
    fn extract_python_imports() {
        let chunks = vec![make_file_chunk(
            "app/auth.py",
            "py",
            &[("from auth import validate_token", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p == "auth"))
            .collect();
        assert!(!imports.is_empty(), "should extract Python from/import");
    }

    #[test]
    fn extract_python_function_calls() {
        let chunks = vec![make_file_chunk(
            "app/auth.py",
            "py",
            &[("result = validate_token(tok)", 5)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let fn_calls: Vec<_> = symbols
            .iter()
            .filter(
                |s| matches!(&s.kind, SymbolKind::FunctionCall(name) if name == "validate_token"),
            )
            .collect();
        assert!(
            !fn_calls.is_empty(),
            "should extract validate_token from Python"
        );
    }

    // --- JavaScript/TypeScript extraction ---

    #[test]
    fn extract_js_imports() {
        let chunks = vec![make_file_chunk(
            "src/api.ts",
            "ts",
            &[("import { validate } from './auth';", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p == "./auth"))
            .collect();
        assert!(!imports.is_empty(), "should extract JS/TS import path");
    }

    // --- Go extraction ---

    #[test]
    fn extract_go_imports() {
        let chunks = vec![make_file_chunk(
            "main.go",
            "go",
            &[("import \"net/http\"", 3)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::Import(p) if p == "net/http"))
            .collect();
        assert!(!imports.is_empty(), "should extract Go import");
    }

    // --- Edge cases ---

    #[test]
    fn skips_binary_files() {
        let chunks = vec![FileChunk {
            old_path: Some("image.png".to_string()),
            new_path: Some("image.png".to_string()),
            language: "image".to_string(),
            chunks: vec![],
            is_binary: true,
            is_deleted: false,
            is_new: false,
        }];
        let symbols = extract_symbols_from_diff(&chunks);
        assert!(symbols.is_empty(), "should skip binary files");
    }

    #[test]
    fn skips_deleted_files() {
        let chunks = vec![FileChunk {
            old_path: Some("old.rs".to_string()),
            new_path: None,
            language: "rs".to_string(),
            chunks: vec![],
            is_binary: false,
            is_deleted: true,
            is_new: false,
        }];
        let symbols = extract_symbols_from_diff(&chunks);
        assert!(symbols.is_empty(), "should skip deleted files");
    }

    #[test]
    fn skips_context_and_removed_lines() {
        let chunks = vec![FileChunk {
            old_path: Some("src/lib.rs".to_string()),
            new_path: Some("src/lib.rs".to_string()),
            language: "rs".to_string(),
            chunks: vec![DiffHunk {
                old_start: 1,
                old_count: 2,
                new_start: 1,
                new_count: 1,
                header: String::new(),
                lines: vec![
                    DiffLine {
                        line_type: DiffLineType::Context,
                        content: "fn old() {}".to_string(),
                        old_line_no: Some(1),
                        new_line_no: Some(1),
                    },
                    DiffLine {
                        line_type: DiffLineType::Remove,
                        content: "fn removed() {}".to_string(),
                        old_line_no: Some(2),
                        new_line_no: None,
                    },
                ],
            }],
            is_binary: false,
            is_deleted: false,
            is_new: false,
        }];
        let symbols = extract_symbols_from_diff(&chunks);
        assert!(symbols.is_empty(), "should only extract from added lines");
    }

    #[test]
    fn deduplicates_across_lines() {
        let chunks = vec![make_file_chunk(
            "src/main.rs",
            "rs",
            &[
                ("let a = foo();", 1),
                ("let b = foo();", 2),
                ("let c = bar();", 3),
            ],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        let foo_count = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::FunctionCall(n) if n == "foo"))
            .count();
        assert_eq!(
            foo_count, 1,
            "should deduplicate same symbol across lines in same file"
        );
    }

    #[test]
    fn respects_max_symbols_per_file() {
        let lines: Vec<(&str, u32)> = (0..60u32)
            .map(|i| {
                let s = format!("let x{i} = func{i}();");
                // Leak to get &'static str
                let leaked: &'static str = Box::leak(s.into_boxed_str());
                (leaked, i + 1)
            })
            .collect();
        let chunks = vec![make_file_chunk("src/big.rs", "rs", &lines)];
        let symbols = extract_symbols_from_diff(&chunks);
        assert!(
            symbols.len() <= MAX_SYMBOLS_PER_FILE,
            "should cap at MAX_SYMBOLS_PER_FILE"
        );
    }

    #[test]
    fn unknown_language_fallback() {
        let chunks = vec![make_file_chunk(
            "README.md",
            "md",
            &[("see foo() for details", 1)],
        )];
        let symbols = extract_symbols_from_diff(&chunks);
        // Fallback should at least extract function calls
        let fn_calls: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(&s.kind, SymbolKind::FunctionCall(n) if n == "foo"))
            .collect();
        assert!(
            !fn_calls.is_empty(),
            "fallback should extract function calls for unknown languages"
        );
    }

    #[test]
    fn full_diff_extraction() {
        let diff = r#"diff --git a/src/auth.rs b/src/auth.rs
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -10,6 +10,8 @@
 use crate::config::CryptoConfig;
+use crate::engine::scanner;
 
 pub fn authenticate(req: &Request) -> Result<bool> {
+    let config = CryptoConfig::load();
+    let valid = validate_token(&req.token, &config);
+    scanner::scan_file(&req.path)
 }
"#;
        let chunks = parse_diff(diff);
        let symbols = extract_symbols_from_diff(&chunks);
        // Should find imports, function calls, and type refs
        assert!(!symbols.is_empty(), "should extract symbols from full diff");
        let has_import = symbols
            .iter()
            .any(|s| matches!(&s.kind, SymbolKind::Import(_)));
        let has_fn_call = symbols
            .iter()
            .any(|s| matches!(&s.kind, SymbolKind::FunctionCall(_)));
        assert!(has_import, "should find import symbols");
        assert!(has_fn_call, "should find function call symbols");
    }
}
