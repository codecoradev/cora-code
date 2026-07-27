//! Symbol extraction from source files.
//!
//! Uses regex-based extraction — same approach as `engine/context/extraction.rs`
//! but extracts definitions (not just references). No heavy AST parser needed.

use regex::Regex;
use std::sync::LazyLock;

use super::symbols::{IndexedSymbol, SymbolKind};

// ─── Rust ───

static RE_RUST_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)").unwrap());

static RE_RUST_STRUCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?struct\s+(\w+)").unwrap());

static RE_RUST_ENUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?enum\s+(\w+)").unwrap());

static RE_RUST_TRAIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?trait\s+(\w+)").unwrap());

static RE_RUST_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?type\s+(\w+)").unwrap());

static RE_RUST_CONST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?(?:const|static)\s+(\w+)").unwrap());

static RE_RUST_MOD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?mod\s+(\w+)").unwrap());

// RE_RUST_IMPL removed — not needed for symbol definitions

// ─── Python ───

static RE_PY_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:async\s+)?def\s+(\w+)").unwrap());

static RE_PY_CLASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*class\s+(\w+)").unwrap());

// ─── TypeScript/JavaScript ───

static RE_TS_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap());

static RE_TS_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)").unwrap());

static RE_TS_INTERFACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:export\s+)?interface\s+(\w+)").unwrap());

static RE_TS_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:export\s+)?type\s+(\w+)").unwrap());

static RE_TS_CONST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:export\s+)?const\s+(\w+)").unwrap());

// ─── Go ───

static RE_GO_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*func\s+(?:\([^)]+\)\s+)?(\w+)").unwrap());

static RE_GO_STRUCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*type\s+(\w+)\s+struct").unwrap());

static RE_GO_INTERFACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*type\s+(\w+)\s+interface").unwrap());

static RE_GO_CONST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*const\s+(\w+)").unwrap());

// ─── Java/Kotlin ───

static RE_JAVA_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:public|private|protected)?\s*(?:abstract\s+)?class\s+(\w+)").unwrap()
});

static RE_JAVA_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:public|private|protected)?\s*(?:static\s+)?[\w<>\[\]]+\s+(\w+)\s*\(")
        .unwrap()
});

// ─── C/C++ ───

static RE_C_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:static\s+)?[\w\*]+\s+(\w+)\s*\([^;]*\)\s*\{").unwrap());

static RE_C_STRUCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:typedef\s+)?struct\s+(\w+)").unwrap());

// ─── Ruby ───

static RE_RB_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*def\s+(?:self\.)?(\w+)").unwrap());

static RE_RB_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:class|module)\s+(\w+)").unwrap());

// ─── PHP ───

static RE_PHP_FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:public|private|protected)?\s*(?:static\s+)?function\s+(\w+)").unwrap()
});

static RE_PHP_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:abstract\s+)?(?:final\s+)?class\s+(\w+)").unwrap());

static RE_PHP_INTERFACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*interface\s+(\w+)").unwrap());

// ─── Swift ───

static RE_SW_FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:public|private|internal|fileprivate)?\s*(?:static\s+)?func\s+(\w+)")
        .unwrap()
});

static RE_SW_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:public|internal|final|open)?\s*(?:class|struct|enum|protocol)\s+(\w+)")
        .unwrap()
});

// ─── Scala ───

static RE_SCALA_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:private|protected)?\s*def\s+(\w+)").unwrap());

static RE_SCALA_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:abstract\s+)?(?:sealed\s+)?(?:class|object|trait|case class)\s+(\w+)")
        .unwrap()
});

// ─── Lua ───

static RE_LUA_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:local\s+)?function\s+(?:[\w.:]+\.)?(\w+)").unwrap());

// ─── Zig ───

static RE_ZIG_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?fn\s+(\w+)").unwrap());

static RE_ZIG_CONST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:pub\s+)?const\s+(\w+)").unwrap());

// ─── Dart ───

static RE_DART_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:abstract\s+)?(?:class|mixin)\s+(\w+)").unwrap());

static RE_DART_ENUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*enum\s+(\w+)").unwrap());

static RE_DART_EXTENDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*extension\s+(?:type\s+)?(\w+)\s+on\s+\w+").unwrap());
static RE_DART_EXTENDS_UNNAMED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*extension\s+(?:type\s+)?on\s+(\w+)").unwrap());

static RE_DART_FN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:static\s+)?(?:external\s+)?(?:@[\w()<>]+\s+)*(?:covariant\s+)?(?:late\s+)?(?:final\s+)?(?:const\s+)?(?:[A-Z]\w*\s+)?(?:Future<[^>]+>\s+)?(?:void|[A-Z]\w*<[^>]+>|[a-zA-Z]\w*(?:<[^>]+>)?)\s+(\w+)\s*\(").unwrap()
});

static RE_DART_TYPEDEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*typedef\s+(\w+)").unwrap());

static RE_DART_GETTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[A-Z]\w*\s+)?(?:[a-zA-Z]\w*(?:<[^>]+>)?)\s+get\s+(\w+)").unwrap()
});
// ─── Svelte ───

static RE_SVELTE_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<script(?:\s[^>]*)?>
?(.*?)
?</script>",
    )
    .unwrap()
});
static RE_SVELTE_EXPORT_PROP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*export\s+let\s+(\w+)").unwrap());
static RE_SVELTE_STATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*let\s+(\w+)\s*=\s*\$state(?:<[^>]*>)?\s*\(").unwrap());
static RE_SVELTE_DERIVED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*let\s+(\w+)\s*=\s*\$derived(?:<[^>]*>)?\s*\(").unwrap());
static RE_SVELTE_EFFECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\$effect\s*\(\s*(?:async\s+)?\(\s*\)\s*=>").unwrap());

/// Extract symbols from source code.
///
/// When the `tree-sitter` feature is enabled, uses AST-based extraction for
/// supported languages (Rust, Go, Python, TypeScript/TSX). Falls back to
/// regex for all other languages.
pub fn extract_symbols(content: &str, language: &str, file_path: &str) -> Vec<ExtractedDef> {
    #[cfg(feature = "tree-sitter")]
    {
        use super::ast;
        if ast::get_language(language).is_some() || language == "svelte" {
            let (nodes, _edges) = ast::extract(content, language, file_path);
            return nodes.into_iter().map(Into::into).collect();
        }
    }

    // Regex fallback for unsupported languages or when feature is disabled
    let mut symbols = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_no = (line_num + 1) as u32;

        match language {
            "rs" => extract_rust(line, line_no, file_path, line, &mut symbols),
            "py" | "pyi" => extract_python(line, line_no, file_path, line, &mut symbols),
            "ts" | "tsx" | "js" | "jsx" => {
                extract_typescript(line, line_no, file_path, line, &mut symbols)
            }
            "go" => extract_go(line, line_no, file_path, line, &mut symbols),
            "java" | "kt" => extract_java(line, line_no, file_path, line, &mut symbols),
            "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => {
                extract_c(line, line_no, file_path, line, &mut symbols)
            }
            "rb" => extract_ruby(line, line_no, file_path, line, &mut symbols),
            "php" => extract_php(line, line_no, file_path, line, &mut symbols),
            "swift" => extract_swift(line, line_no, file_path, line, &mut symbols),
            "scala" => extract_scala(line, line_no, file_path, line, &mut symbols),
            "lua" => extract_lua(line, line_no, file_path, line, &mut symbols),
            "zig" => extract_zig(line, line_no, file_path, line, &mut symbols),
            "dart" => extract_dart(line, line_no, file_path, line, &mut symbols),
            "svelte" => extract_svelte(content, file_path, &mut symbols),
            _ => {}
        }
    }

    // Deduplicate by (name, line) — regex may match multiple times
    symbols.dedup_by(|a, b| a.name == b.name && a.line == b.line);

    symbols
}

/// A symbol definition extracted from source code.
#[derive(Debug, Clone)]
pub struct ExtractedDef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: u32,
    pub signature: String,
}

impl From<&ExtractedDef> for IndexedSymbol {
    fn from(d: &ExtractedDef) -> Self {
        Self {
            id: 0,
            name: d.name.clone(),
            kind: d.kind.clone(),
            file: d.file.clone(),
            line: d.line,
            signature: d.signature.clone(),
            language: String::new(),
        }
    }
}

// ─── Per-language extractors ───

fn extract_rust(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_RUST_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_STRUCT.captures(line) {
        out.push(def(cap, SymbolKind::Struct, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_ENUM.captures(line) {
        out.push(def(cap, SymbolKind::Enum, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_TRAIT.captures(line) {
        out.push(def(cap, SymbolKind::Trait, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_TYPE.captures(line) {
        out.push(def(cap, SymbolKind::TypeAlias, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_CONST.captures(line) {
        out.push(def(cap, SymbolKind::Constant, line_no, file, raw));
    }
    if let Some(cap) = RE_RUST_MOD.captures(line) {
        out.push(def(cap, SymbolKind::Module, line_no, file, raw));
    }
}

fn extract_python(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_PY_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_PY_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
}

fn extract_typescript(
    line: &str,
    line_no: u32,
    file: &str,
    raw: &str,
    out: &mut Vec<ExtractedDef>,
) {
    if let Some(cap) = RE_TS_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_TS_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
    if let Some(cap) = RE_TS_INTERFACE.captures(line) {
        out.push(def(cap, SymbolKind::Interface, line_no, file, raw));
    }
    if let Some(cap) = RE_TS_TYPE.captures(line) {
        out.push(def(cap, SymbolKind::TypeAlias, line_no, file, raw));
    }
    if let Some(cap) = RE_TS_CONST.captures(line) {
        out.push(def(cap, SymbolKind::Constant, line_no, file, raw));
    }
}

fn extract_go(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_GO_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_GO_STRUCT.captures(line) {
        out.push(def(cap, SymbolKind::Struct, line_no, file, raw));
    }
    if let Some(cap) = RE_GO_INTERFACE.captures(line) {
        out.push(def(cap, SymbolKind::Interface, line_no, file, raw));
    }
    if let Some(cap) = RE_GO_CONST.captures(line) {
        out.push(def(cap, SymbolKind::Constant, line_no, file, raw));
    }
}

fn extract_java(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_JAVA_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
    if let Some(cap) = RE_JAVA_METHOD.captures(line) {
        out.push(def(cap, SymbolKind::Method, line_no, file, raw));
    }
}

fn extract_c(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_C_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_C_STRUCT.captures(line) {
        out.push(def(cap, SymbolKind::Struct, line_no, file, raw));
    }
}

fn extract_ruby(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_RB_FN.captures(line) {
        out.push(def(cap, SymbolKind::Method, line_no, file, raw));
    }
    if let Some(cap) = RE_RB_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
}

fn extract_php(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_PHP_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_PHP_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
    if let Some(cap) = RE_PHP_INTERFACE.captures(line) {
        out.push(def(cap, SymbolKind::Interface, line_no, file, raw));
    }
}

fn extract_swift(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_SW_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_SW_TYPE.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
}

fn extract_scala(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_SCALA_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_SCALA_TYPE.captures(line) {
        out.push(def(cap, SymbolKind::Class, line_no, file, raw));
    }
}

fn extract_lua(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_LUA_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
}

fn extract_zig(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_ZIG_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_ZIG_CONST.captures(line) {
        out.push(def(cap, SymbolKind::Constant, line_no, file, raw));
    }
}

fn extract_dart(line: &str, line_no: u32, file: &str, raw: &str, out: &mut Vec<ExtractedDef>) {
    if let Some(cap) = RE_DART_CLASS.captures(line) {
        out.push(def(cap, SymbolKind::Struct, line_no, file, raw));
    }
    if let Some(cap) = RE_DART_ENUM.captures(line) {
        out.push(def(cap, SymbolKind::Enum, line_no, file, raw));
    }
    if let Some(cap) = RE_DART_EXTENDS.captures(line) {
        out.push(def(cap, SymbolKind::Trait, line_no, file, raw));
    } else if let Some(cap) = RE_DART_EXTENDS_UNNAMED.captures(line) {
        out.push(def(cap, SymbolKind::Trait, line_no, file, raw));
    }
    if let Some(cap) = RE_DART_FN.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
    if let Some(cap) = RE_DART_TYPEDEF.captures(line) {
        out.push(def(cap, SymbolKind::TypeAlias, line_no, file, raw));
    }
    if let Some(cap) = RE_DART_GETTER.captures(line) {
        out.push(def(cap, SymbolKind::Function, line_no, file, raw));
    }
}

/// Extract Svelte symbols:
/// 1. Derive component name from filename (PascalCase or kebab → PascalCase)
/// 2. Strip `<script>` blocks and delegate to TS/JS extractor
/// 3. Extract Svelte-specific: export props, $state, $derived
fn extract_svelte(content: &str, file: &str, out: &mut Vec<ExtractedDef>) {
    // 1. Component name from filename
    let component_name = std::path::Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| {
            let mut result = String::new();
            let mut capitalize_next = true;
            for ch in s.chars() {
                if ch == '-' || ch == '_' {
                    capitalize_next = true;
                } else if capitalize_next {
                    result.push(ch.to_ascii_uppercase());
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
            }
            result
        })
        .unwrap_or_default();

    if !component_name.is_empty() {
        out.push(ExtractedDef {
            name: component_name.clone(),
            kind: SymbolKind::Struct,
            file: file.to_string(),
            line: 1,
            signature: format!("<!-- {} component -->", component_name),
        });
    }

    // 2. Extract <script> content and delegate to TS/JS extractor
    if let Some(cap) = RE_SVELTE_SCRIPT.captures(content) {
        let script_content = cap.get(1).map(|m| m.as_str()).unwrap_or("");

        for (line_num, line) in script_content.lines().enumerate() {
            let line_no = (line_num + 1) as u32;
            extract_typescript(line, line_no, file, line, out);

            if let Some(cap) = RE_SVELTE_EXPORT_PROP.captures(line) {
                out.push(def(cap, SymbolKind::Variable, line_no, file, line));
            }
            if let Some(cap) = RE_SVELTE_STATE.captures(line) {
                out.push(def(cap, SymbolKind::Variable, line_no, file, line));
            }
            if let Some(cap) = RE_SVELTE_DERIVED.captures(line) {
                out.push(def(cap, SymbolKind::Variable, line_no, file, line));
            }
            if RE_SVELTE_EFFECT.is_match(line) {
                out.push(ExtractedDef {
                    name: "$effect".to_string(),
                    kind: SymbolKind::Function,
                    file: file.to_string(),
                    line: line_no,
                    signature: line.trim().to_string(),
                });
            }
        }

        // Dedup: TS extractor may produce duplicates for Svelte-specific symbols
        let mut seen = std::collections::HashSet::new();
        out.retain(|s| {
            let key = (s.name.clone(), s.line);
            seen.insert(key)
        });
    }
}

/// Extract function call sites from source code.
///
/// When the `tree-sitter` feature is enabled, uses AST-based extraction for
/// supported languages. Falls back to regex-based scope tracking for others.
pub fn extract_calls(content: &str, language: &str, file_path: &str) -> Vec<CallSite> {
    #[cfg(feature = "tree-sitter")]
    {
        use super::ast;
        if ast::get_language(language).is_some() || language == "svelte" {
            let (_nodes, edges) = ast::extract(content, language, file_path);
            return edges
                .into_iter()
                .filter(|e| e.kind == ast::EdgeKind::Calls)
                .map(Into::into)
                .collect();
        }
    }

    // Regex fallback
    use crate::engine::context::extraction as ctx_extract;
    use crate::engine::context::types::SymbolKind as CtxSymbolKind;

    let lines: Vec<&str> = content.lines().collect();
    let mut current_fn: Option<String> = None;
    let mut brace_depth: i32 = 0;
    let mut calls = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let line_no = (i + 1) as u32;

        // Track current function scope for Rust/Go/C/Java
        if language == "rs"
            || language == "go"
            || language == "c"
            || language == "cpp"
            || language == "java"
            || language == "kt"
            || language == "dart"
        {
            // Check if we're entering a function
            if let Some(fn_name) = detect_function_entry(line, language) {
                current_fn = Some(fn_name);
                brace_depth = 0;
            }
        }

        // Track brace depth for scope
        brace_depth += line.chars().filter(|&c| c == '{').count() as i32
            - line.chars().filter(|&c| c == '}').count() as i32;
        if brace_depth <= 0 && current_fn.is_some() {
            current_fn = None;
        }

        // Extract function calls from this line
        let symbols = ctx_extract::extract_symbols_from_line(line, language);
        for sym in symbols {
            if let CtxSymbolKind::FunctionCall(name) = sym {
                // Skip self-references and builtins
                if name == current_fn.as_deref().unwrap_or("") {
                    continue;
                }
                if is_builtin(&name) {
                    continue;
                }
                if let Some(caller) = &current_fn {
                    calls.push(CallSite {
                        caller: caller.clone(),
                        callee: name,
                        file: file_path.to_string(),
                        line: line_no,
                    });
                }
            }
        }
    }

    calls
}

/// Extract all graph edges from source code (calls, imports, implements, inherits).
///
/// Only available with the `tree-sitter` feature. Returns empty for unsupported languages.
#[cfg(feature = "tree-sitter")]
#[allow(dead_code)]
pub fn extract_edges(
    content: &str,
    language: &str,
    file_path: &str,
) -> Vec<crate::index::ast::AstEdge> {
    use super::ast;
    if ast::get_language(language).is_some() || language == "svelte" {
        let (_nodes, edges) = ast::extract(content, language, file_path);
        return edges;
    }
    Vec::new()
}

/// Detect function entry from a line (returns function name).
fn detect_function_entry(line: &str, language: &str) -> Option<String> {
    let trimmed = line.trim();

    match language {
        "rs" => {
            static RE: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*[(<]").unwrap());
            RE.captures(trimmed)
                .map(|c| c.get(1).unwrap().as_str().to_string())
        }
        "go" => {
            static RE: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"func\s+(?:\([^)]+\)\s+)?(\w+)\s*\(").unwrap());
            RE.captures(trimmed)
                .map(|c| c.get(1).unwrap().as_str().to_string())
        }
        "c" | "cpp" | "h" | "hpp" => {
            static RE: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"[\w:*]+\s+(\w+)\s*\([^;]*\)\s*\{").unwrap());
            RE.captures(trimmed)
                .map(|c| c.get(1).unwrap().as_str().to_string())
        }
        "java" | "kt" | "dart" => {
            static RE: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"\b(\w+)\s*\([^)]*\)\s*\{").unwrap());
            RE.captures(trimmed)
                .map(|c| c.get(1).unwrap().as_str().to_string())
        }
        "py" => {
            static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"def\s+(\w+)").unwrap());
            RE.captures(trimmed)
                .map(|c| c.get(1).unwrap().as_str().to_string())
        }
        _ => None,
    }
}

/// Check if a name is a builtin/keyword to skip.
fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "match"
            | "return"
            | "break"
            | "continue"
            | "print"
            | "println"
            | "eprintln"
            | "format"
            | "write"
            | "writeln"
            | "vec"
            | "Box"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "Result"
            | "Option"
            | "String"
            | "str"
            | "Vec"
            | "HashMap"
            | "HashSet"
            | "dbg"
            | "todo"
            | "unimplemented"
            | "unreachable"
            | "panic"
            | "assert"
            | "assert_eq"
            | "assert_ne"
            | "len"
            | "is_empty"
            | "push"
            | "pop"
            | "insert"
            | "remove"
            | "get"
            | "set"
            | "new"
            | "default"
            | "clone"
            | "into"
            | "from"
            | "iter"
            | "collect"
            | "map"
            | "filter"
            | "fold"
            | "next"
            | "unwrap"
            | "expect"
            | "as_ref"
            | "as_mut"
            | "as_str"
            | "to_string"
            | "to_owned"
            | "to_vec"
            | "drop"
            | "copy"
            | "send"
            | "sync"
            | "main"
            // Dart builtins
            | "debugPrint"
            | "throw"
            | "rethrow"
            | "late"
            | "required"
            | "covariant"
            | "show"
            | "hide"
            | "Future"
            | "Stream"
            | "Completer"
            | "List"
            | "Map"
            | "Set"
            | "int"
            | "double"
            | "num"
            | "bool"
            | "dynamic"
            | "Object"
            | "Null"
            | "Never"
    )
}

/// A function call site extracted from source code.
#[derive(Debug, Clone)]
pub struct CallSite {
    pub caller: String,
    pub callee: String,
    pub file: String,
    pub line: u32,
}

fn def(cap: regex::Captures, kind: SymbolKind, line: u32, file: &str, raw: &str) -> ExtractedDef {
    ExtractedDef {
        name: cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
        kind,
        line,
        file: file.to_string(),
        signature: raw.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust() {
        let code = r#"
pub struct Cache {
    inner: HashMap<String, String>,
}

impl Cache {
    pub fn new() -> Self {
        Self {}
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.inner.get(key)
    }
}

enum Status {
    Active,
    Inactive,
}

const MAX_SIZE: usize = 100;
"#;
        let symbols = extract_symbols(code, "rs", "src/cache.rs");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        eprintln!(
            "GO symbols: {:?}",
            symbols
                .iter()
                .map(|s| (&s.kind, &s.name))
                .collect::<Vec<_>>()
        );

        assert!(names.contains(&"Cache"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"get"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"MAX_SIZE"));
    }

    #[test]
    fn test_extract_python() {
        let code = r#"
class AuthService:
    def __init__(self):
        self.secret = ""

    async def validate(self, token: str) -> bool:
        return False
"#;
        let symbols = extract_symbols(code, "py", "auth.py");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"AuthService"));
        assert!(names.contains(&"validate"));
    }

    #[test]
    fn test_extract_typescript() {
        let code = r#"
export interface User {
    id: string;
    name: string;
}

export class UserService {
    async getUser(id: string): Promise<User> {
        return {} as User;
    }
}

export type Status = 'active' | 'inactive';

export const DEFAULT_TIMEOUT = 5000;
"#;
        let symbols = extract_symbols(code, "ts", "user.ts");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"User"));
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"DEFAULT_TIMEOUT"));
    }

    #[test]
    fn test_extract_go() {
        let code = r#"
type Server struct {
    port int
}

func (s *Server) Start() error {
    return nil
}

func NewServer(port int) *Server {
    return &Server{port: port}
}

const DefaultPort = 8080
"#;
        let symbols = extract_symbols(code, "go", "server.go");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"Server"));
        assert!(names.contains(&"Start"));
        assert!(names.contains(&"NewServer"));
        assert!(names.contains(&"DefaultPort"));
    }

    #[test]
    fn test_extract_unknown_language() {
        let symbols = extract_symbols("fn test() {}", "unknown", "test.txt");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_extract_empty() {
        let symbols = extract_symbols("", "rs", "empty.rs");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_extract_ruby() {
        let code = r#"
class ApplicationController
  def authenticate_user
    @current_user
  end
end

module Auth
  def validate_token(token)
    false
  end
end
"#;
        let symbols = extract_symbols(code, "rb", "app.rb");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ApplicationController"));
        assert!(names.contains(&"authenticate_user"));
        assert!(names.contains(&"Auth"));
    }

    #[test]
    fn test_extract_php() {
        let code = r#"
<?php
class UserController {
    public function show($id) {
        return $this->find($id);
    }
}

interface Repository {
    public function find($id);
}
"#;
        let symbols = extract_symbols(code, "php", "user.php");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserController"));
        assert!(names.contains(&"show"));
        assert!(names.contains(&"Repository"));
    }

    #[test]
    fn test_extract_swift() {
        let code = r#"
public struct User {
    var id: String
}

func authenticate(token: String) -> Bool {
    return false
}

enum AuthError: Error {
    case invalidToken
}
"#;
        let symbols = extract_symbols(code, "swift", "auth.swift");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"User"));
        assert!(names.contains(&"authenticate"));
    }

    #[test]
    fn test_extract_lua() {
        let code = r#"
local function validate(input)
    return true
end

function M.handler(req)
    return validate(req.body)
end
"#;
        let symbols = extract_symbols(code, "lua", "handler.lua");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"validate"));
        assert!(names.contains(&"handler"));
    }

    #[test]
    fn test_extract_zig() {
        let code = r#"
pub fn main() !void {
    try run();
}

const MAX_RETRIES: u32 = 3;
"#;
        let symbols = extract_symbols(code, "zig", "main.zig");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"MAX_RETRIES"));
    }

    #[test]
    fn test_extract_dart() {
        let code = r#"
class UserService {
  String? name;
  void login(String email) {
    print(email);
  }

  static Future<void> fetchData() async {
    return Future.value();
  }

  String get displayName => name ?? '';

  set displayName(String value) {}
}

enum Status { active, inactive }

extension UserServiceHelpers on UserService {
  String greet() => "Hello";
}

typedef UserCallback = void Function(User);

mixin Validator {
  bool validate(String input) => true;
}

abstract class Shape {
  double area();
}
"#;
        let symbols = extract_symbols(code, "dart", "lib/models/user.dart");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        let kinds: Vec<(String, &str)> = symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind.as_str()))
            .collect();
        eprintln!("Dart symbols: {:?}", kinds);

        assert!(names.contains(&"UserService"), "UserService class");
        assert!(names.contains(&"Shape"), "Shape abstract class");
        assert!(names.contains(&"Validator"), "Validator mixin");
        assert!(names.contains(&"login"), "login method");
        assert!(names.contains(&"fetchData"), "fetchData async method");
        assert!(names.contains(&"displayName"), "displayName getter");
        assert!(names.contains(&"greet"), "extension method greet");
        assert!(names.contains(&"validate"), "mixin method validate");
        assert!(names.contains(&"area"), "abstract method area");
        assert!(names.contains(&"Status"), "Status enum");
        assert!(
            names.contains(&"UserServiceHelpers"),
            "extension on UserService"
        );
        assert!(names.contains(&"UserCallback"), "typedef UserCallback");
    }

    #[test]
    fn test_extract_dart_issue_373_repro() {
        let code = r#"class UserService {
  String? name;
  void login(String email) {
    print(email);
  }
}

Future<void> fetchData() async {
  return Future.value();
}

enum Status { active, inactive }"#;
        let symbols = extract_symbols(code, "dart", "sample.dart");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(!symbols.is_empty(), "Should extract at least 1 Dart symbol");
        assert!(names.contains(&"UserService"), "UserService class");
        assert!(names.contains(&"login"), "login method");
        assert!(names.contains(&"fetchData"), "fetchData function");
        assert!(names.contains(&"Status"), "Status enum");
    }

    #[test]
    fn test_extract_svelte() {
        let content = r#"<script lang="ts">
  import { onMount } from 'svelte';
  export let name: string = 'world';
  export let count: number = 0;
  let doubled = $derived(count * 2);
  let items = $state<string[]>([]);

  function handleClick(): void {
    count++;
  }

  async function fetchData(): Promise<void> {
    const res = await fetch('/api');
  }

  onMount(() => {
    console.log('mounted');
  });
</script>

<button onclick={handleClick}>
  {name} — count: {count}
</button>

{#if count > 0}
  <p>Counting!</p>
{/if}

<style>
  button { padding: 8px; }
</style>
"#;

        let symbols = extract_symbols(content, "svelte", "src/lib/Counter.svelte");
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        println!("Svelte symbols: {:?}", names);

        // AST-based extraction captures function declarations and arrow functions
        // from <script> blocks. Svelte-specific syntax ($state, $derived, props)
        // is NOT captured by the TS grammar — that's expected.
        assert!(
            names.contains(&"handleClick"),
            "missing function: handleClick"
        );
        assert!(names.contains(&"fetchData"), "missing function: fetchData");
    }
}
