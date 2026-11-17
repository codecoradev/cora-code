// ! Tree-sitter AST extraction.
// !
// ! Provides proper AST-based symbol and edge extraction using tree-sitter.
// ! Only compiled when the `tree-sitter` feature is enabled.

use crate::index::extract::CallSite;
use crate::index::extract::ExtractedDef;
use crate::index::symbols::SymbolKind;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// A node in the code graph — a definition extracted from source.
#[derive(Debug, Clone)]
pub struct AstNode {
    /// Symbol name.
    pub name: String,
    /// Kind of definition.
    pub kind: SymbolKind,
    /// Source file path.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// Full signature text.
    pub signature: String,
    /// Parent scope name (struct/class for methods).
    #[allow(dead_code)] // computed at every site, never read (#792)
    pub parent: Option<String>,
}

impl From<&AstNode> for ExtractedDef {
    fn from(n: &AstNode) -> Self {
        Self {
            name: n.name.clone(),
            kind: n.kind.clone(),
            file: n.file.clone(),
            line: n.line,
            signature: n.signature.clone(),
        }
    }
}

impl From<AstNode> for ExtractedDef {
    fn from(n: AstNode) -> Self {
        ExtractedDef::from(&n)
    }
}

/// Edge type in the knowledge graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeKind {
    Calls,
    Imports,
    Implements,
    Inherits,
    Route,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "CALLS",
            Self::Imports => "IMPORTS",
            Self::Implements => "IMPLEMENTS",
            Self::Inherits => "INHERITS",
            Self::Route => "ROUTE",
        }
    }
}

/// An edge in the code graph.
#[derive(Debug, Clone)]
pub struct AstEdge {
    pub source: String,
    pub kind: EdgeKind,
    pub target: String,
    pub file: String,
    pub line: u32,
    /// For call edges: the type or module the callee was reached through
    /// (`WebSearchTool` in `WebSearchTool::from_env()`, the enclosing impl type
    /// for `self.m()` and `Self::m()`). `None` when it is not statically
    /// obvious, which makes the edge ambiguous (#714).
    pub qualifier: Option<String>,
    /// For call edges: the impl type of the calling method, when it is one.
    pub caller_type: Option<String>,
}

impl From<&AstEdge> for CallSite {
    fn from(e: &AstEdge) -> Self {
        Self {
            caller: e.source.clone(),
            callee: e.target.clone(),
            file: e.file.clone(),
            line: e.line,
            qualifier: e.qualifier.clone(),
            caller_type: e.caller_type.clone(),
        }
    }
}

impl From<AstEdge> for CallSite {
    fn from(e: AstEdge) -> Self {
        CallSite::from(&e)
    }
}

// ─── Public API ─────────────────────────────────────────────────────────

/// Get the tree-sitter Language for a file extension.
/// Returns `None` for unsupported languages.
pub fn get_language(ext: &str) -> Option<tree_sitter::Language> {
    match ext {
        // Original 5
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "py" | "pyi" => Some(tree_sitter_python::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        // Batch 2 — 8 new languages
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "cs" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "rb" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        "scala" | "sc" => Some(tree_sitter_scala::LANGUAGE.into()),
        "js" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}

/// Parse source code into a tree-sitter tree.
pub fn parse(source: &[u8], language: tree_sitter::Language) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

/// Extract definitions and edges using tree-sitter.
///
/// Returns `(nodes, edges)`. Empty vectors for unsupported languages
/// or parse failures.
pub fn extract(content: &str, language: &str, file_path: &str) -> (Vec<AstNode>, Vec<AstEdge>) {
    // Svelte: extract <script> blocks and parse as TS/JS (no dedicated grammar needed)
    if language == "svelte" {
        return extract_svelte(content, file_path);
    }

    let lang = match get_language(language) {
        Some(l) => l,
        None => return (Vec::new(), Vec::new()),
    };

    let tree = match parse(content.as_bytes(), lang) {
        Some(t) => t,
        None => return (Vec::new(), Vec::new()),
    };

    match language {
        "rs" => extract_rust(&tree.root_node(), content, file_path),
        "go" => extract_go(&tree.root_node(), content, file_path),
        "py" | "pyi" => extract_python(&tree.root_node(), content, file_path),
        "ts" | "tsx" | "js" | "mjs" | "cjs" => {
            extract_typescript(&tree.root_node(), content, file_path)
        }
        "java" => extract_java(&tree.root_node(), content, file_path),
        "c" | "h" => extract_c(&tree.root_node(), content, file_path),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => extract_cpp(&tree.root_node(), content, file_path),
        "cs" => extract_csharp(&tree.root_node(), content, file_path),
        "rb" => extract_ruby(&tree.root_node(), content, file_path),
        "php" => extract_php(&tree.root_node(), content, file_path),
        "scala" | "sc" => extract_scala(&tree.root_node(), content, file_path),
        _ => (Vec::new(), Vec::new()),
    }
}
// ─── Helpers ───────────────────────────────────────────────────────────

fn node_text(node: &tree_sitter::Node, source: &str) -> String {
    node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
}

fn signature_for_node(node: &tree_sitter::Node, source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start_row = node.start_position().row;
    let end_row = node.end_position().row;
    let start_col = node.start_position().column;
    let end_col = node.end_position().column;
    if start_row >= lines.len() {
        return String::new();
    }
    let first = &lines[start_row][start_col..];
    if start_row == end_row {
        first[..std::cmp::min(end_col, first.len())].to_string()
    } else if end_row < lines.len() {
        let last = &lines[end_row][..std::cmp::min(end_col, lines[end_row].len())];
        format!("{first}\n...\n{last}")
    } else {
        first.to_string()
    }
}

/// Find the first child node matching one of the given kinds.
fn find_child_by_kind<'a>(
    parent: &tree_sitter::Node<'a>,
    kinds: &[&str],
) -> Option<tree_sitter::Node<'a>> {
    let mut c = parent.walk();
    if c.goto_first_child() {
        loop {
            if kinds.contains(&c.node().kind()) {
                return Some(c.node());
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Get the "name" of a definition node — tries field name first, then kind-based fallback.
fn node_name(node: &tree_sitter::Node, source: &str) -> String {
    if let Some(n) = node.child_by_field_name("name") {
        return node_text(&n, source);
    }
    if let Some(n) = find_child_by_kind(
        node,
        &[
            "identifier",
            "type_identifier",
            "field_identifier",
            "name",
            "qualified_name",
        ],
    ) {
        return node_text(&n, source);
    }
    String::new()
}

/// Find the first type_identifier or generic_type descendant in a node (for extends clauses).
fn first_type_identifier(node: &tree_sitter::Node, source: &str) -> String {
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            let n = c.node();
            if n.kind() == "type_identifier"
                || n.kind() == "generic_type"
                || n.kind() == "identifier"
            {
                return node_text(&n, source);
            }
            // Recurse one level for nested expressions
            if let Some(inner) =
                find_child_by_kind(&n, &["type_identifier", "generic_type", "identifier"])
            {
                return node_text(&inner, source);
            }
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
    String::new()
}

/// Get the type/trait target from a node (for impl, type_spec, etc.).
fn node_type(node: &tree_sitter::Node, source: &str) -> String {
    if let Some(n) = node.child_by_field_name("type") {
        return node_text(&n, source);
    }
    if let Some(n) = find_child_by_kind(node, &["type_identifier"]) {
        return node_text(&n, source);
    }
    String::new()
}

/// Get the trait/interface name from an impl node.
fn node_trait(node: &tree_sitter::Node, source: &str) -> String {
    if let Some(n) = node.child_by_field_name("trait") {
        return node_text(&n, source);
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        while c.goto_next_sibling() {
            let n = c.node();
            if n.kind() == "type_identifier" {
                return node_text(&n, source);
            }
            if n.kind() == "declaration_list" || n.kind() == "for" {
                break;
            }
        }
    }
    String::new()
}

// ─── Rust ──────────────────────────────────────────────────────────────

fn extract_rust(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                    extract_calls_from_node(&child, source, file_path, &name, &mut edges);
                }
            }
            "struct_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    serde_attribute_calls(&child, source, file_path, &name, &mut edges);
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Struct,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "enum_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    serde_attribute_calls(&child, source, file_path, &name, &mut edges);
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "trait_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Trait,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "type_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::TypeAlias,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "const_item" | "static_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Constant,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "mod_item" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Module,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "impl_item" => {
                let trait_n = node_trait(&child, source);
                let type_n = node_type(&child, source);
                if !trait_n.is_empty() && !type_n.is_empty() {
                    edges.push(AstEdge {
                        source: type_n.clone(),
                        kind: EdgeKind::Implements,
                        target: trait_n,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        qualifier: None,
                        caller_type: None,
                    });
                }
                // Extract methods inside impl block
                let mut mc = child.walk();
                if mc.goto_first_child() {
                    loop {
                        if mc.node().kind() == "declaration_list" {
                            let mut dc = mc.node().walk();
                            if dc.goto_first_child() {
                                loop {
                                    let gc = dc.node();
                                    if gc.kind() == "function_item" {
                                        let name = node_name(&gc, source);
                                        if !name.is_empty() {
                                            nodes.push(AstNode {
                                                name: name.clone(),
                                                kind: SymbolKind::Function,
                                                file: file_path.to_string(),
                                                line: (gc.start_position().row + 1) as u32,
                                                signature: signature_for_node(&gc, source),
                                                parent: Some(type_n.clone()),
                                            });
                                            // Walk impl-method bodies for call
                                            // edges too — without this,
                                            // cross-crate callers of these
                                            // methods are invisible (#519).
                                            extract_calls_in_impl(
                                                &gc,
                                                source,
                                                file_path,
                                                &name,
                                                Some(&type_n)
                                                    .filter(|t| !t.is_empty())
                                                    .map(|t| t.as_str()),
                                                &mut edges,
                                            );
                                        }
                                    }
                                    if !dc.goto_next_sibling() {
                                        break;
                                    }
                                }
                            }
                        }
                        if !mc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "use_declaration" => {
                if let Some(arg) = child.child_by_field_name("argument") {
                    edges.push(AstEdge {
                        source: file_path.to_string(),
                        kind: EdgeKind::Imports,
                        target: node_text(&arg, source),
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        qualifier: None,
                        caller_type: None,
                    });
                }
            }
            _ => {}
        }
    }
    (nodes, edges)
}

/// Reduce a call target to its final name segment so it joins against symbol
/// names: method calls (`self.export_full`, `manager.remember()`) and
/// qualified paths (`std::mem::drop`) all become their bare final name (#519).
fn normalize_callee_name(raw: &str) -> &str {
    raw.rsplit(['.', ':'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(raw)
}

/// Walk function body for call expressions using cursor-based DFS.
fn extract_calls_from_node(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    caller: &str,
    edges: &mut Vec<AstEdge>,
) {
    extract_calls_in_impl(node, source, file_path, caller, None, edges);
}

/// Last path segment of a type or module path with generics removed:
/// `crate::web::WebTool<T>` -> `WebTool`. `None` for `<T as Trait>` forms,
/// which do not name one type.
fn path_qualifier(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with('<') {
        return None;
    }
    // `Type::<u8>` (turbofish) names `Type`.
    let raw = raw.split("::<").next()?;
    let last = raw.rsplit("::").next()?;
    let name = last.split('<').next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The type or module a call is made through, when the source says so:
/// `Type::f()` and `module::f()` give `Type` / `module`, `Self::f()` and
/// `self.f()` give the enclosing impl type. A method call on any other value
/// has an unknown receiver type and gives `None` (ambiguous, #714).
fn callee_qualifier(
    fn_node: &tree_sitter::Node,
    source: &str,
    self_type: Option<&str>,
) -> Option<String> {
    match fn_node.kind() {
        "scoped_identifier" => {
            let q = path_qualifier(&node_text(&fn_node.child_by_field_name("path")?, source))?;
            if q == "Self" {
                self_type.map(str::to_string)
            } else if matches!(q.as_str(), "crate" | "super" | "self") {
                None
            } else {
                Some(q)
            }
        }
        "field_expression" => {
            let value = fn_node.child_by_field_name("value")?;
            (value.kind() == "self")
                .then(|| self_type.map(str::to_string))
                .flatten()
        }
        "generic_function" => {
            callee_qualifier(&fn_node.child_by_field_name("function")?, source, self_type)
        }
        _ => None,
    }
}

/// `extract_calls_from_node` with the enclosing impl type, so `self.m()` and
/// `Self::m()` resolve to it.
fn extract_calls_in_impl(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    caller: &str,
    self_type: Option<&str>,
    edges: &mut Vec<AstEdge>,
) {
    fn walk_calls(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        caller: &str,
        self_type: Option<&str>,
        locals: &HashSet<String>,
        edges: &mut Vec<AstEdge>,
    ) {
        if node.kind() == "call_expression" {
            if let Some(fn_node) = node.child_by_field_name("function") {
                let raw = node_text(&fn_node, source);
                let callee = normalize_callee_name(&raw);
                if !callee.is_empty() {
                    edges.push(AstEdge {
                        source: caller.to_string(),
                        kind: EdgeKind::Calls,
                        target: callee.to_string(),
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        qualifier: callee_qualifier(&fn_node, source, self_type),
                        caller_type: self_type.map(str::to_string),
                    });
                }
            }
        }
        // `.map(helper)`, `.map(Type::make)`: a function passed as a value is called
        // by the function that passes it, although there is no `call_expression`.
        if node.kind() == "arguments" {
            function_references(node, source, file_path, caller, self_type, locals, edges);
        }
        // The arguments of a macro are a raw `token_tree`, not parsed
        // expressions, so a call inside `json!(..)`, `vec![..]`, `format!(..)`,
        // `assert!(..)` has no `call_expression` node (#715).
        if node.kind() == "token_tree" {
            token_tree_calls(node, source, file_path, caller, self_type, edges);
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                walk_calls(
                    &cursor.node(),
                    source,
                    file_path,
                    caller,
                    self_type,
                    locals,
                    edges,
                );
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    let locals = local_bindings(&node_text(node, source));
    walk_calls(node, source, file_path, caller, self_type, &locals, edges);
}

/// Names a function body binds itself (parameters, `let`, `for`, closure
/// parameters). A bare identifier with one of these names that is passed as an
/// argument is a variable, not a function used as a value.
fn local_bindings(body: &str) -> HashSet<String> {
    // Every lowercase word of a pattern is a binding or a harmless keyword.
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[a-z_]\w*").unwrap());
    // `let (a, b) =`, `let mut x =`, `if let Some(v) =`, `for (i, x) in`
    static AFTER_KEYWORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\b(?:let|for)\b([^=;{]*?)(?:=|\bin\b)").unwrap());
    // `Some(v) =>`, `Ok(value) if cond =>`
    static ARM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[A-Z]\w*\s*\(([^()]*)\)\s*(?:if\b[^=\n]*)?=>").unwrap());
    // `Command::Callers { symbol, limit } =>`, `if let Cfg { name, .. } =`
    static STRUCT_PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b[A-Z][\w:]*\s*\{([^{}]*)\}\s*(?:=>|=)").unwrap());
    // `name: Type` (parameters, closure parameters with a type)
    static TYPED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([a-z_]\w*)\s*:").unwrap());
    // `|a, b|`
    static CLOSURE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\|([^|]*)\|").unwrap());
    let mut names = HashSet::new();
    for re in [&*AFTER_KEYWORD, &*ARM, &*STRUCT_PATTERN, &*CLOSURE] {
        for cap in re.captures_iter(body) {
            names.extend(WORD.find_iter(&cap[1]).map(|m| m.as_str().to_string()));
        }
    }
    names.extend(TYPED.captures_iter(body).map(|c| c[1].to_string()));
    names
}

/// Function paths passed as arguments: a bare snake_case identifier that the
/// function does not bind itself, or a `Type::f` / `module::f` path. Constants,
/// enum variants and types start uppercase and are skipped.
fn function_references(
    args: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    caller: &str,
    self_type: Option<&str>,
    locals: &HashSet<String>,
    edges: &mut Vec<AstEdge>,
) {
    let mut cursor = args.walk();
    for arg in args.children(&mut cursor) {
        if !matches!(arg.kind(), "identifier" | "scoped_identifier") {
            continue;
        }
        let name = normalize_callee_name(&node_text(&arg, source)).to_string();
        if !name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        if arg.kind() == "identifier" && locals.contains(&name) {
            continue;
        }
        edges.push(AstEdge {
            source: caller.to_string(),
            kind: EdgeKind::Calls,
            target: name,
            file: file_path.to_string(),
            line: (arg.start_position().row + 1) as u32,
            qualifier: callee_qualifier(&arg, source, self_type),
            caller_type: self_type.map(str::to_string),
        });
    }
}

/// Functions named in the `serde` attributes of a struct or enum and of its
/// fields (`default = "f"`, `deserialize_with = "path::f"`, ...): serde calls
/// them, so the item is their caller.
fn serde_attribute_calls(
    item: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    owner: &str,
    edges: &mut Vec<AstEdge>,
) {
    static SERDE_FN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"\b(?:default|deserialize_with|serialize_with|skip_serializing_if|with)\s*=\s*"([A-Za-z_][\w:]*)""#,
        )
        .unwrap()
    });
    if item.kind() == "attribute_item" {
        let text = node_text(item, source);
        if text.contains("serde") {
            for cap in SERDE_FN.captures_iter(&text) {
                let path = &cap[1];
                let name = path.rsplit("::").next().unwrap_or(path);
                if name.is_empty() {
                    continue;
                }
                let qualifier = path.rsplit_once("::").and_then(|(prefix, _)| {
                    path_qualifier(prefix)
                        .filter(|q| !matches!(q.as_str(), "crate" | "super" | "self"))
                });
                edges.push(AstEdge {
                    source: owner.to_string(),
                    kind: EdgeKind::Calls,
                    target: name.to_string(),
                    file: file_path.to_string(),
                    line: (item.start_position().row + 1) as u32,
                    qualifier,
                    caller_type: None,
                });
            }
        }
        return;
    }
    let mut cursor = item.walk();
    for child in item.children(&mut cursor) {
        serde_attribute_calls(&child, source, file_path, owner, edges);
    }
}

/// Items that a token tree can declare; an identifier after one of these
/// followed by `(` is a definition (`fn helper()` inside a `macro_rules!`
/// body), not a call.
const DECLARING_TOKENS: &[&str] = &["fn", "struct", "enum", "union", "trait", "mod", "type"];

/// Call edges for `name(` patterns among the direct children of a `token_tree`
/// (macro arguments): an identifier immediately followed by a parenthesised
/// token tree. Nested token trees are visited by the caller's walk, so
/// `json!({ "k": f(x) })` reaches `f` at the inner level. A path (`a::b(x)`)
/// yields its last segment, a method call (`x.m()`) the method name, the same
/// as `normalize_callee_name` does for real call expressions.
fn token_tree_calls(
    tree: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    caller: &str,
    self_type: Option<&str>,
    edges: &mut Vec<AstEdge>,
) {
    let mut cursor = tree.walk();
    let children: Vec<tree_sitter::Node> = tree.children(&mut cursor).collect();
    for (i, pair) in children.windows(2).enumerate() {
        let (name, args) = (&pair[0], &pair[1]);
        if name.kind() != "identifier" || args.kind() != "token_tree" {
            continue;
        }
        if !node_text(args, source).starts_with('(') {
            continue;
        }
        let declared = i
            .checked_sub(1)
            .map(|p| node_text(&children[p], source))
            .is_some_and(|prev| DECLARING_TOKENS.contains(&prev.as_str()));
        if declared {
            continue;
        }
        let callee = node_text(name, source);
        // `Type :: name (` — the two tokens before the name are `::` and the
        // type (`Self` resolves to the enclosing impl type).
        let qualifier = i
            .checked_sub(2)
            .filter(|&q| node_text(&children[q + 1], source) == "::")
            .map(|q| node_text(&children[q], source))
            .filter(|t| children[i - 2].kind() == "identifier" || t == "Self")
            .and_then(|t| {
                if t == "Self" {
                    self_type.map(str::to_string)
                } else if matches!(t.as_str(), "crate" | "super" | "self") {
                    None
                } else {
                    Some(t)
                }
            });
        if !callee.is_empty() {
            edges.push(AstEdge {
                source: caller.to_string(),
                kind: EdgeKind::Calls,
                target: callee,
                file: file_path.to_string(),
                line: (name.start_position().row + 1) as u32,
                qualifier,
                caller_type: self_type.map(str::to_string),
            });
        }
    }
}

// ─── Go ───────────────────────────────────────────────────────────────

fn extract_go(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "method_declaration" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    let kind = if child.kind() == "method_declaration" {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                    extract_calls_from_node(&child, source, file_path, &name, &mut edges);
                }
            }
            "type_declaration" => {
                let mut tc = child.walk();
                if tc.goto_first_child() {
                    loop {
                        if tc.node().kind() == "type_spec" {
                            let name = node_name(&tc.node(), source);
                            let type_node = tc.node().child_by_field_name("type");
                            let kind = type_node
                                .as_ref()
                                .map(|t| match t.kind() {
                                    "struct_type" => SymbolKind::Struct,
                                    "interface_type" => SymbolKind::Interface,
                                    _ => SymbolKind::TypeAlias,
                                })
                                .unwrap_or(SymbolKind::TypeAlias);
                            if !name.is_empty() {
                                nodes.push(AstNode {
                                    name,
                                    kind,
                                    file: file_path.to_string(),
                                    line: (tc.node().start_position().row + 1) as u32,
                                    signature: signature_for_node(&tc.node(), source),
                                    parent: None,
                                });
                            }
                        }
                        if !tc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "import_declaration" => {
                if let Some(path_node) = child.child_by_field_name("path") {
                    edges.push(AstEdge {
                        source: file_path.to_string(),
                        kind: EdgeKind::Imports,
                        target: node_text(&path_node, source),
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        qualifier: None,
                        caller_type: None,
                    });
                }
            }
            "const_declaration" => {
                let mut cc = child.walk();
                if cc.goto_first_child() {
                    loop {
                        if cc.node().kind() == "const_spec" {
                            let name = node_name(&cc.node(), source);
                            if !name.is_empty() {
                                nodes.push(AstNode {
                                    name,
                                    kind: SymbolKind::Constant,
                                    file: file_path.to_string(),
                                    line: (cc.node().start_position().row + 1) as u32,
                                    signature: signature_for_node(&cc.node(), source),
                                    parent: None,
                                });
                            }
                        }
                        if !cc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "var_declaration" => {
                let mut vc = child.walk();
                if vc.goto_first_child() {
                    loop {
                        if vc.node().kind() == "var_spec" {
                            let name = node_name(&vc.node(), source);
                            if !name.is_empty() {
                                nodes.push(AstNode {
                                    name,
                                    kind: SymbolKind::Constant,
                                    file: file_path.to_string(),
                                    line: (vc.node().start_position().row + 1) as u32,
                                    signature: signature_for_node(&vc.node(), source),
                                    parent: None,
                                });
                            }
                        }
                        if !vc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (nodes, edges)
}

// ─── Python ─────────────────────────────────────────────────────────

fn extract_python(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                    extract_calls_from_node(&child, source, file_path, &name, &mut edges);
                }
            }
            "class_definition" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    let line = (child.start_position().row + 1) as u32;
                    // Check for parent classes (inheritance)
                    if let Some(bases) = child.child_by_field_name("superclasses") {
                        let mut bc = bases.walk();
                        if bc.goto_first_child() {
                            loop {
                                let base_name = node_text(&bc.node(), source);
                                if !base_name.is_empty() {
                                    edges.push(AstEdge {
                                        source: name.clone(),
                                        kind: EdgeKind::Inherits,
                                        target: base_name,
                                        file: file_path.to_string(),
                                        line,
                                        qualifier: None,
                                        caller_type: None,
                                    });
                                }
                                if !bc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                    // Extract methods inside class body
                    if let Some(body) = find_child_by_kind(&child, &["block"]) {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                if mc.node().kind() == "function_definition" {
                                    let mname = node_name(&mc.node(), source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mc.node().start_position().row + 1) as u32,
                                            signature: signature_for_node(&mc.node(), source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "import_statement" | "import_from_statement" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(&child, source),
                    file: file_path.to_string(),
                    line: (child.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            _ => {}
        }
    }
    (nodes, edges)
}

// ─── TypeScript ─────────────────────────────────────────────────────

// ─── TypeScript ─────────────────────────────────────────────────────

fn extract_typescript(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    fn process_ts_node(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "function_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "class_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    // Inheritance: class Foo extends Bar
                    // tree-sitter-ts wraps extends/implements in class_heritage
                    let heritage = find_child_by_kind(node, &["class_heritage"]);
                    // Some grammars put extends_clause directly on class_declaration
                    let extends_direct = find_child_by_kind(node, &["extends_clause"]);
                    if let Some(heritage) = heritage {
                        let mut hc = heritage.walk();
                        if hc.goto_first_child() {
                            loop {
                                let hn = hc.node();
                                if hn.kind() == "extends_clause" {
                                    // First type_identifier in extends = parent class
                                    let parent = first_type_identifier(&hn, source);
                                    if !parent.is_empty() {
                                        edges.push(AstEdge {
                                            source: name.clone(),
                                            kind: EdgeKind::Inherits,
                                            target: parent,
                                            file: file_path.to_string(),
                                            line,
                                            qualifier: None,
                                            caller_type: None,
                                        });
                                    }
                                } else if hn.kind() == "implements_clause" {
                                    // All type_identifiers in implements = interfaces
                                    let mut tc = hn.walk();
                                    if tc.goto_first_child() {
                                        loop {
                                            if tc.node().kind() == "type_identifier"
                                                || tc.node().kind() == "generic_type"
                                                || tc.node().kind() == "identifier"
                                            {
                                                let iface = node_text(&tc.node(), source);
                                                if !iface.is_empty() {
                                                    edges.push(AstEdge {
                                                        source: name.clone(),
                                                        kind: EdgeKind::Implements,
                                                        target: iface,
                                                        file: file_path.to_string(),
                                                        line,
                                                        qualifier: None,
                                                        caller_type: None,
                                                    });
                                                }
                                            }
                                            if !tc.goto_next_sibling() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                if !hc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    } else if let Some(extends_clause) = extends_direct {
                        // Direct extends_clause (no class_heritage wrapper)
                        let parent = first_type_identifier(&extends_clause, source);
                        if !parent.is_empty() {
                            edges.push(AstEdge {
                                source: name.clone(),
                                kind: EdgeKind::Inherits,
                                target: parent,
                                file: file_path.to_string(),
                                line,
                                qualifier: None,
                                caller_type: None,
                            });
                        }
                        // Also check for implements_clause sibling
                        if let Some(impl_clause) = find_child_by_kind(node, &["implements_clause"])
                        {
                            let mut tc = impl_clause.walk();
                            if tc.goto_first_child() {
                                loop {
                                    if tc.node().kind() == "type_identifier"
                                        || tc.node().kind() == "generic_type"
                                        || tc.node().kind() == "identifier"
                                    {
                                        let iface = node_text(&tc.node(), source);
                                        if !iface.is_empty() {
                                            edges.push(AstEdge {
                                                source: name.clone(),
                                                kind: EdgeKind::Implements,
                                                target: iface,
                                                file: file_path.to_string(),
                                                line,
                                                qualifier: None,
                                                caller_type: None,
                                            });
                                        }
                                    }
                                    if !tc.goto_next_sibling() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    if let Some(body) = find_child_by_kind(node, &["class_body"]) {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                if mc.node().kind() == "method_definition" {
                                    let mname = node_name(&mc.node(), source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mc.node().start_position().row + 1) as u32,
                                            signature: signature_for_node(&mc.node(), source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "interface_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Interface,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "type_alias_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::TypeAlias,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "lexical_declaration" => {
                // const/let declarations — extract constants AND function-valued vars
                let mut vc = node.walk();
                if vc.goto_first_child() {
                    loop {
                        if vc.node().kind() == "variable_declarator" {
                            let decl_node = vc.node();
                            let name = node_name(&decl_node, source);
                            if name.is_empty() {
                                if !vc.goto_next_sibling() {
                                    break;
                                }
                                continue;
                            }
                            // Check if the value is a function (arrow or function expression)
                            let is_function = decl_node
                                .child_by_field_name("value")
                                .map(|v| {
                                    v.kind() == "arrow_function"
                                        || v.kind() == "function_expression"
                                })
                                .unwrap_or(false);

                            if is_function {
                                // Exported arrow function / function expression
                                nodes.push(AstNode {
                                    name: name.clone(),
                                    kind: SymbolKind::Function,
                                    file: file_path.to_string(),
                                    line: (decl_node.start_position().row + 1) as u32,
                                    signature: signature_for_node(&decl_node, source),
                                    parent: None,
                                });
                                // Extract calls from the function body
                                if let Some(value_node) = decl_node.child_by_field_name("value") {
                                    extract_calls_from_node(
                                        &value_node,
                                        source,
                                        file_path,
                                        &name,
                                        edges,
                                    );
                                }
                            } else if name == name.to_uppercase() && name.len() > 1 {
                                // ALL_CAPS constant
                                nodes.push(AstNode {
                                    name,
                                    kind: SymbolKind::Constant,
                                    file: file_path.to_string(),
                                    line: (decl_node.start_position().row + 1) as u32,
                                    signature: signature_for_node(&decl_node, source),
                                    parent: None,
                                });
                            }
                        }
                        if !vc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "variable_declaration" => {
                let mut vc = node.walk();
                if vc.goto_first_child() {
                    loop {
                        if vc.node().kind() == "variable_declarator" {
                            let decl_node = vc.node();
                            let name = node_name(&decl_node, source);
                            if name.is_empty() {
                                if !vc.goto_next_sibling() {
                                    break;
                                }
                                continue;
                            }
                            let is_function = decl_node
                                .child_by_field_name("value")
                                .map(|v| {
                                    v.kind() == "arrow_function"
                                        || v.kind() == "function_expression"
                                })
                                .unwrap_or(false);

                            if is_function {
                                nodes.push(AstNode {
                                    name: name.clone(),
                                    kind: SymbolKind::Function,
                                    file: file_path.to_string(),
                                    line: (decl_node.start_position().row + 1) as u32,
                                    signature: signature_for_node(&decl_node, source),
                                    parent: None,
                                });
                                if let Some(value_node) = decl_node.child_by_field_name("value") {
                                    extract_calls_from_node(
                                        &value_node,
                                        source,
                                        file_path,
                                        &name,
                                        edges,
                                    );
                                }
                            } else if name == name.to_uppercase() && name.len() > 1 {
                                nodes.push(AstNode {
                                    name,
                                    kind: SymbolKind::Constant,
                                    file: file_path.to_string(),
                                    line: (decl_node.start_position().row + 1) as u32,
                                    signature: signature_for_node(&decl_node, source),
                                    parent: None,
                                });
                            }
                        }
                        if !vc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "export_statement" => {
                // Unwrap: recurse into the exported declaration
                let mut ec = node.walk();
                if ec.goto_first_child() {
                    loop {
                        let inner = ec.node();
                        if inner.kind() != "export" && inner.kind() != ";" {
                            process_ts_node(&inner, source, file_path, nodes, edges);
                        }
                        if !ec.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "import_statement" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            _ => {}
        }
    }

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        process_ts_node(&child, source, file_path, &mut nodes, &mut edges);
    }

    (nodes, edges)
}

// ─── Svelte ──────────────────────────────────────────────────────────

/// Extract `<script>` blocks from Svelte source, parse as TypeScript/JavaScript,
/// and adjust line numbers to match the original file.
///
/// Svelte files look like:
/// ```svelte
/// <script lang="ts">
///   import { foo } from './foo';
///   export const handler = () => { foo(); };
///   function bar() {}
/// </script>
///
/// <div>...</div>
/// ```
///
/// We extract the script content, determine the language from `lang="ts|js"`,
/// parse with the TS/JS grammar, then offset all line numbers by the script
/// block's start line.
fn extract_svelte(content: &str, file_path: &str) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();

    for script_block in extract_script_blocks(content) {
        let script_content = &script_block.content;
        let line_offset = script_block.start_line;

        // Determine language: default to "ts" (SvelteKit convention), use "js" if explicitly set
        let lang = if script_block.lang == "js" || script_block.lang == "javascript" {
            "js"
        } else {
            "ts"
        };

        let tree_sitter_lang = match get_language(lang) {
            Some(l) => l,
            None => continue,
        };

        let tree = match parse(script_content.as_bytes(), tree_sitter_lang) {
            Some(t) => t,
            None => continue,
        };

        let (mut nodes, mut edges) =
            extract_typescript(&tree.root_node(), script_content, file_path);

        // Adjust line numbers: tree-sitter rows are 0-based, we add 1 for display.
        // The script content starts at `line_offset` (0-based) in the original file.
        // So: original_line = script_local_line + line_offset
        for node in &mut nodes {
            node.line += line_offset as u32;
        }
        for edge in &mut edges {
            edge.line += line_offset as u32;
        }

        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }

    (all_nodes, all_edges)
}

/// A extracted `<script>` block with its metadata.
struct ScriptBlock {
    content: String,
    start_line: usize, // 0-based line offset in the original file
    lang: String,      // "ts", "js", "javascript", or empty
}

/// Extract all `<script>` blocks from Svelte source.
/// Returns blocks with their content, start line offset, and lang attribute.
fn extract_script_blocks(content: &str) -> Vec<ScriptBlock> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = content.lines().collect();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        // Look for <script ...> tag (allow attributes like lang="ts")
        if let Some(tag_end) = find_script_open_tag(line) {
            // Extract lang attribute from the tag line
            let lang = extract_lang_attr(line);

            // Handle inline: <script>code</script> on one line
            if let Some(close_pos) = rest_of_line_find_close(line, tag_end) {
                let inline_content = &line[tag_end..close_pos];
                if !inline_content.trim().is_empty() {
                    blocks.push(ScriptBlock {
                        content: inline_content.to_string(),
                        start_line: i,
                        lang,
                    });
                }
                i += 1;
                continue;
            }

            // Multi-line script block
            let mut script_lines = Vec::new();
            let mut start_line = i + 1; // content starts on the next line (0-based)
            let mut j = i + 1;

            // If content starts on the same line as the tag
            if tag_end < line.len() {
                let rest = &line[tag_end..];
                if !rest.trim().is_empty() {
                    script_lines.push(rest.to_string());
                    start_line = i; // content starts on same line as tag
                }
            }

            let mut found_close = false;
            while j < lines.len() {
                let j_line = lines[j];
                if let Some(close_pos) = j_line.find("</script>") {
                    // Content before </script> on this line
                    let before = &j_line[..close_pos];
                    if !before.trim().is_empty() {
                        script_lines.push(before.to_string());
                    }
                    found_close = true;
                    break;
                } else {
                    script_lines.push(j_line.to_string());
                }
                j += 1;
            }

            if found_close && !script_lines.is_empty() {
                blocks.push(ScriptBlock {
                    content: script_lines.join("\n"),
                    start_line,
                    lang,
                });
            }

            i = j + 1;
        } else {
            i += 1;
        }
    }

    blocks
}

/// Find the position after the opening `<script ...>` tag on a line.
/// Returns the byte offset after `>`, or None if no script tag.
fn find_script_open_tag(line: &str) -> Option<usize> {
    let lower = line.to_lowercase();
    let pos = lower.find("<script")?;
    // Find the closing > after <script
    let after_tag = &line[pos..];
    after_tag.find('>').map(|p| pos + p + 1)
}

/// Extract the `lang` attribute value from a `<script lang="...">` tag.
fn extract_lang_attr(line: &str) -> String {
    let lower = line.to_lowercase();
    if let Some(lang_pos) = lower.find("lang=") {
        let after = &line[lang_pos + 5..];
        // Skip optional whitespace
        let after = after.trim_start();
        if let Some(inner) = after.strip_prefix('"') {
            let end = inner.find('"').map(|p| p + 1).unwrap_or(inner.len());
            return inner[..end].to_string();
        } else if let Some(inner) = after.strip_prefix('\'') {
            let end = inner.find('\'').map(|p| p + 1).unwrap_or(inner.len());
            return inner[..end].to_string();
        }
    }
    String::new()
}

/// Check if `</script>` appears on the same line as `<script>` after `start_pos`.
/// Returns the position of `</script>` if found.
fn rest_of_line_find_close(line: &str, start_pos: usize) -> Option<usize> {
    let after = &line[start_pos..];
    let lower = after.to_lowercase();
    lower.find("</script>").map(|p| start_pos + p)
}

// ─── Java ─────────────────────────────────────────────────────────

fn extract_java(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    fn walk_java(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "class_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    // Inheritance
                    if let Some(superclass) = node.child_by_field_name("superclass") {
                        let parent = node_text(&superclass, source);
                        if !parent.is_empty() {
                            edges.push(AstEdge {
                                source: name.clone(),
                                kind: EdgeKind::Inherits,
                                target: parent,
                                file: file_path.to_string(),
                                line,
                                qualifier: None,
                                caller_type: None,
                            });
                        }
                    }
                    // Interfaces
                    if let Some(interfaces) = node.child_by_field_name("interfaces") {
                        let mut ic = interfaces.walk();
                        if ic.goto_first_child() {
                            loop {
                                let iface = node_text(&ic.node(), source);
                                if !iface.is_empty() {
                                    edges.push(AstEdge {
                                        source: name.clone(),
                                        kind: EdgeKind::Implements,
                                        target: iface,
                                        file: file_path.to_string(),
                                        line,
                                        qualifier: None,
                                        caller_type: None,
                                    });
                                }
                                if !ic.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Methods inside class body
                    if let Some(body) = find_child_by_kind(node, &["class_body", "block"]) {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                let mn = mc.node();
                                if mn.kind() == "method_declaration"
                                    || mn.kind() == "constructor_declaration"
                                {
                                    let mname = node_name(&mn, source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mn.start_position().row + 1) as u32,
                                            signature: signature_for_node(&mn, source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                } else if mn.kind() == "field_declaration" {
                                    let mut fc = mn.walk();
                                    if fc.goto_first_child() {
                                        loop {
                                            if fc.node().kind() == "variable_declarator" {
                                                let fname = node_name(&fc.node(), source);
                                                if !fname.is_empty() {
                                                    nodes.push(AstNode {
                                                        name: fname,
                                                        kind: SymbolKind::Variable,
                                                        file: file_path.to_string(),
                                                        line: (fc.node().start_position().row + 1)
                                                            as u32,
                                                        signature: signature_for_node(
                                                            &fc.node(),
                                                            source,
                                                        ),
                                                        parent: Some(name.clone()),
                                                    });
                                                }
                                            }
                                            if !fc.goto_next_sibling() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "interface_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Interface,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "method_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "import_declaration" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            "enum_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_java(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

// ─── C ─────────────────────────────────────────────────────────────

fn extract_c(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                    extract_calls_from_node(&child, source, file_path, &name, &mut edges);
                }
            }
            "struct_specifier" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Struct,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "enum_specifier" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "type_definition" => {
                let name = node_name(&child, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::TypeAlias,
                        file: file_path.to_string(),
                        line: (child.start_position().row + 1) as u32,
                        signature: signature_for_node(&child, source),
                        parent: None,
                    });
                }
            }
            "preproc_include" => {
                let mut pc = child.walk();
                if pc.goto_first_child() {
                    loop {
                        if pc.node().kind() == "string_literal"
                            || pc.node().kind() == "system_lib_string"
                        {
                            let inc = node_text(&pc.node(), source).trim_matches('"').to_string();
                            if !inc.is_empty() {
                                edges.push(AstEdge {
                                    source: file_path.to_string(),
                                    kind: EdgeKind::Imports,
                                    target: inc,
                                    file: file_path.to_string(),
                                    line: (child.start_position().row + 1) as u32,
                                    qualifier: None,
                                    caller_type: None,
                                });
                            }
                            break;
                        }
                        if !pc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (nodes, edges)
}

// ─── C++ ───────────────────────────────────────────────────────────

fn extract_cpp(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    fn walk_cpp(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "function_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "class_specifier" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    // Inheritance
                    if let Some(bases) = node.child_by_field_name("base_list_clause") {
                        let mut bc = bases.walk();
                        if bc.goto_first_child() {
                            loop {
                                if bc.node().kind() == "type_identifier" {
                                    let parent = node_text(&bc.node(), source);
                                    if !parent.is_empty() {
                                        edges.push(AstEdge {
                                            source: name.clone(),
                                            kind: EdgeKind::Inherits,
                                            target: parent,
                                            file: file_path.to_string(),
                                            line,
                                            qualifier: None,
                                            caller_type: None,
                                        });
                                    }
                                }
                                if !bc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Methods inside class body
                    if let Some(body) =
                        find_child_by_kind(node, &["field_declaration_list", "class_body"])
                    {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                let mn = mc.node();
                                if mn.kind() == "function_definition" {
                                    let mname = node_name(&mn, source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mn.start_position().row + 1) as u32,
                                            signature: signature_for_node(&mn, source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "struct_specifier" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Struct,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "enum_specifier" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "namespace_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Module,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Recurse into namespace body
                    if let Some(body) = find_child_by_kind(node, &["declaration_list"]) {
                        let mut nc = body.walk();
                        if nc.goto_first_child() {
                            loop {
                                walk_cpp(&nc.node(), source, file_path, nodes, edges);
                                if !nc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "template_declaration" => {
                // Unwrap template and recurse into inner declaration
                let mut tc = node.walk();
                if tc.goto_first_child() {
                    loop {
                        let inner = tc.node();
                        if inner.kind() != "template" && inner.kind() != ">" {
                            walk_cpp(&inner, source, file_path, nodes, edges);
                        }
                        if !tc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            "preproc_include" => {
                let mut pc = node.walk();
                if pc.goto_first_child() {
                    loop {
                        if pc.node().kind() == "string_literal"
                            || pc.node().kind() == "system_lib_string"
                        {
                            let inc = node_text(&pc.node(), source).trim_matches('"').to_string();
                            if !inc.is_empty() {
                                edges.push(AstEdge {
                                    source: file_path.to_string(),
                                    kind: EdgeKind::Imports,
                                    target: inc,
                                    file: file_path.to_string(),
                                    line: (node.start_position().row + 1) as u32,
                                    qualifier: None,
                                    caller_type: None,
                                });
                            }
                            break;
                        }
                        if !pc.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_cpp(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

// ─── C# ────────────────────────────────────────────────────────────

fn extract_csharp(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    fn walk_csharp(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "class_declaration"
            | "struct_declaration"
            | "record_declaration"
            | "interface_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    let kind = match node.kind() {
                        "interface_declaration" => SymbolKind::Interface,
                        "struct_declaration" => SymbolKind::Struct,
                        "record_declaration" => SymbolKind::Struct,
                        _ => SymbolKind::Class,
                    };
                    // Inheritance / implementation
                    if let Some(bases) = node.child_by_field_name("bases") {
                        let mut bc = bases.walk();
                        if bc.goto_first_child() {
                            loop {
                                let base = node_text(&bc.node(), source);
                                if !base.is_empty() {
                                    // Heuristic: base type starting with 'I' is likely an interface
                                    let ek = if base.starts_with('I')
                                        && base
                                            .chars()
                                            .nth(1)
                                            .is_some_and(|c| c.is_ascii_uppercase())
                                    {
                                        EdgeKind::Implements
                                    } else {
                                        EdgeKind::Inherits
                                    };
                                    edges.push(AstEdge {
                                        source: name.clone(),
                                        kind: ek,
                                        target: base,
                                        file: file_path.to_string(),
                                        line,
                                        qualifier: None,
                                        caller_type: None,
                                    });
                                }
                                if !bc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Members
                    if let Some(body) = find_child_by_kind(node, &["declaration_list"]) {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                let mn = mc.node();
                                if mn.kind() == "method_declaration"
                                    || mn.kind() == "constructor_declaration"
                                {
                                    let mname = node_name(&mn, source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mn.start_position().row + 1) as u32,
                                            signature: signature_for_node(&mn, source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                } else if mn.kind() == "property_declaration" {
                                    let pname = node_name(&mn, source);
                                    if !pname.is_empty() {
                                        nodes.push(AstNode {
                                            name: pname,
                                            kind: SymbolKind::Variable,
                                            file: file_path.to_string(),
                                            line: (mn.start_position().row + 1) as u32,
                                            signature: signature_for_node(&mn, source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "enum_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "method_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "namespace_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Module,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    if let Some(body) = find_child_by_kind(node, &["declaration_list"]) {
                        let mut nc = body.walk();
                        if nc.goto_first_child() {
                            loop {
                                walk_csharp(&nc.node(), source, file_path, nodes, edges);
                                if !nc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "using_directive" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_csharp(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

// ─── Ruby ──────────────────────────────────────────────────────────

/// Walk a Ruby class/module node to extract method definitions.
/// Handles the tree-sitter-ruby `body_statement` wrapper that sits
/// between the class/module node and its method children.
fn extract_methods_from_ruby_node(
    parent: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    parent_name: &str,
    nodes: &mut Vec<AstNode>,
) {
    let mut mc = parent.walk();
    if mc.goto_first_child() {
        loop {
            let kind = mc.node().kind();
            if kind == "method" {
                let mname = if let Some(n) = mc.node().child_by_field_name("name") {
                    node_text(&n, source)
                } else {
                    String::new()
                };
                if !mname.is_empty() {
                    nodes.push(AstNode {
                        name: mname,
                        kind: SymbolKind::Method,
                        file: file_path.to_string(),
                        line: (mc.node().start_position().row + 1) as u32,
                        signature: signature_for_node(&mc.node(), source),
                        parent: Some(parent_name.to_string()),
                    });
                }
            } else if kind == "body_statement" || kind == "singleton_class" {
                // tree-sitter-ruby wraps class/module contents in body_statement;
                // recurse one level to find methods inside.
                let mut inner = mc.node().walk();
                if inner.goto_first_child() {
                    loop {
                        if inner.node().kind() == "method" {
                            let mname = if let Some(n) = inner.node().child_by_field_name("name") {
                                node_text(&n, source)
                            } else {
                                String::new()
                            };
                            if !mname.is_empty() {
                                nodes.push(AstNode {
                                    name: mname,
                                    kind: SymbolKind::Method,
                                    file: file_path.to_string(),
                                    line: (inner.node().start_position().row + 1) as u32,
                                    signature: signature_for_node(&inner.node(), source),
                                    parent: Some(parent_name.to_string()),
                                });
                            }
                        }
                        if !inner.goto_next_sibling() {
                            break;
                        }
                    }
                }
            }
            if !mc.goto_next_sibling() {
                break;
            }
        }
    }
}

fn extract_ruby(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    /// Visit the classes and modules nested in a class or module body (the usual
    /// `module Outer; class Inner; def run` layout, #791). Only `class` and `module`
    /// children are walked: their methods are already taken by
    /// `extract_methods_from_ruby_node`, and walking a `method` again would index it twice.
    fn walk_ruby_nested(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "class" | "module" => walk_ruby(&child, source, file_path, nodes, edges),
                // tree-sitter-ruby wraps the body of a class or module in body_statement.
                "body_statement" => walk_ruby_nested(&child, source, file_path, nodes, edges),
                _ => {}
            }
        }
    }

    fn walk_ruby(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "class" => {
                let name = if let Some(n) = node.child_by_field_name("name") {
                    node_text(&n, source)
                } else {
                    String::new()
                };
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    // Parent class
                    if let Some(superclass) = node.child_by_field_name("superclass") {
                        // The superclass node's text is `< Base`; keep the name only, or the
                        // edge never matches the symbol `Base`.
                        let parent = node_text(&superclass, source)
                            .trim_start_matches('<')
                            .trim()
                            .to_string();
                        if !parent.is_empty() {
                            edges.push(AstEdge {
                                source: name.clone(),
                                kind: EdgeKind::Inherits,
                                target: parent,
                                file: file_path.to_string(),
                                line,
                                qualifier: None,
                                caller_type: None,
                            });
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Methods inside class (tree-sitter-ruby wraps them in body_statement)
                    extract_methods_from_ruby_node(node, source, file_path, &name, nodes);
                    walk_ruby_nested(node, source, file_path, nodes, edges);
                }
            }
            "module" => {
                let name = if let Some(n) = node.child_by_field_name("name") {
                    node_text(&n, source)
                } else {
                    String::new()
                };
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Module,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Methods inside module (same body_statement wrapping)
                    extract_methods_from_ruby_node(node, source, file_path, &name, nodes);
                    walk_ruby_nested(node, source, file_path, nodes, edges);
                }
            }
            "method" => {
                let name = if let Some(n) = node.child_by_field_name("name") {
                    node_text(&n, source)
                } else {
                    String::new()
                };
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "require" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_ruby(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

// ─── PHP ───────────────────────────────────────────────────────────

fn extract_php(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    fn walk_php(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "class_declaration" | "anonymous_class_creation_expression" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    // Inheritance: class Foo extends Bar
                    // PHP grammar uses base_clause child (no "superclass" field)
                    if let Some(base) = find_child_by_kind(node, &["base_clause"]) {
                        let parent = node_name(&base, source);
                        if !parent.is_empty() {
                            edges.push(AstEdge {
                                source: name.clone(),
                                kind: EdgeKind::Inherits,
                                target: parent,
                                file: file_path.to_string(),
                                line,
                                qualifier: None,
                                caller_type: None,
                            });
                        }
                    }
                    // Interfaces: class Foo implements Bar, Baz
                    // PHP grammar uses class_interface_clause child
                    if let Some(ifaces) = find_child_by_kind(node, &["class_interface_clause"]) {
                        let mut ic = ifaces.walk();
                        if ic.goto_first_child() {
                            loop {
                                let cn = ic.node();
                                // PHP interface names are `name` or `qualified_name` nodes
                                if cn.kind() == "name" || cn.kind() == "qualified_name" {
                                    let iface = node_text(&cn, source);
                                    if !iface.is_empty() {
                                        edges.push(AstEdge {
                                            source: name.clone(),
                                            kind: EdgeKind::Implements,
                                            target: iface,
                                            file: file_path.to_string(),
                                            line,
                                            qualifier: None,
                                            caller_type: None,
                                        });
                                    }
                                }
                                if !ic.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Class,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    // Methods
                    if let Some(body) =
                        find_child_by_kind(node, &["declaration_list", "class_body"])
                    {
                        let mut mc = body.walk();
                        if mc.goto_first_child() {
                            loop {
                                if mc.node().kind() == "method_declaration" {
                                    let mname = node_name(&mc.node(), source);
                                    if !mname.is_empty() {
                                        nodes.push(AstNode {
                                            name: mname,
                                            kind: SymbolKind::Method,
                                            file: file_path.to_string(),
                                            line: (mc.node().start_position().row + 1) as u32,
                                            signature: signature_for_node(&mc.node(), source),
                                            parent: Some(name.clone()),
                                        });
                                    }
                                }
                                if !mc.goto_next_sibling() {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            "interface_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Interface,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "function_definition" | "method_declaration" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "namespace_definition" => {
                let name = if let Some(n) = node.child_by_field_name("name") {
                    node_text(&n, source)
                } else {
                    String::new()
                };
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Module,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "use_declaration" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_php(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

// ─── Scala ─────────────────────────────────────────────────────────

fn extract_scala(
    root: &tree_sitter::Node,
    source: &str,
    file_path: &str,
) -> (Vec<AstNode>, Vec<AstEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut cursor = root.walk();

    fn walk_scala(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "class_definition" | "object_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    let line = (node.start_position().row + 1) as u32;
                    let kind = if node.kind() == "object_definition" {
                        SymbolKind::Module
                    } else {
                        SymbolKind::Class
                    };
                    // Inheritance and trait mixing: class Foo extends Bar with Baz
                    if let Some(extends_clause) = find_child_by_kind(node, &["extends_clause"]) {
                        // First type = parent class (Inherits)
                        if let Some(base) = find_child_by_kind(
                            &extends_clause,
                            &["type_identifier", "generic_type", "class_type"],
                        ) {
                            let parent = node_text(&base, source);
                            if !parent.is_empty() {
                                edges.push(AstEdge {
                                    source: name.clone(),
                                    kind: EdgeKind::Inherits,
                                    target: parent,
                                    file: file_path.to_string(),
                                    line,
                                    qualifier: None,
                                    caller_type: None,
                                });
                            }
                        }
                    }
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind,
                        file: file_path.to_string(),
                        line,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "trait_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Trait,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            "function_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name: name.clone(),
                        kind: SymbolKind::Function,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                    extract_calls_from_node(node, source, file_path, &name, edges);
                }
            }
            "import_declaration" => {
                edges.push(AstEdge {
                    source: file_path.to_string(),
                    kind: EdgeKind::Imports,
                    target: signature_for_node(node, source),
                    file: file_path.to_string(),
                    line: (node.start_position().row + 1) as u32,
                    qualifier: None,
                    caller_type: None,
                });
            }
            "enum_definition" => {
                let name = node_name(node, source);
                if !name.is_empty() {
                    nodes.push(AstNode {
                        name,
                        kind: SymbolKind::Enum,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                        signature: signature_for_node(node, source),
                        parent: None,
                    });
                }
            }
            _ => {}
        }
    }

    for child in root.children(&mut cursor) {
        walk_scala(&child, source, file_path, &mut nodes, &mut edges);
    }
    (nodes, edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_language_supported() {
        assert!(get_language("rs").is_some());
        assert!(get_language("go").is_some());
        assert!(get_language("py").is_some());
        assert!(get_language("ts").is_some());
        assert!(get_language("tsx").is_some());
        assert!(get_language("pyi").is_some());
    }

    #[test]
    fn test_get_language_unsupported() {
        // Previously unsupported, now all supported via tree-sitter
        assert!(get_language("rb").is_some());
        assert!(get_language("java").is_some());
        assert!(get_language("cs").is_some());
        assert!(get_language("scala").is_some());
        assert!(get_language("php").is_some());
        assert!(get_language("").is_none());
    }

    #[test]
    fn test_extract_rust_functions() {
        let code = r#"pub fn hello() -> String {
    "hello".to_string()
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}"#;
        let (nodes, edges) = extract(code, "rs", "test.rs");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"add"));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Calls));
    }

    #[test]
    fn test_extract_rust_struct_and_impl() {
        let code = r#"pub struct Cache {
    data: HashMap<String, String>,
}

impl Cache {
    pub fn new() -> Self {
        Self { data: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.data.get(key)
    }
}

pub trait Store {
    fn save(&self, data: &str);
}

impl Store for Cache {
    fn save(&self, data: &str) {
        todo!()
    }
}"#;
        let (nodes, edges) = extract(code, "rs", "cache.rs");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Cache"));
        assert!(names.contains(&"Store"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"get"));
        assert!(names.contains(&"save"));
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::Implements
                && e.source == "Cache"
                && e.target == "Store")
        );
    }

    #[test]
    fn test_extract_go_functions() {
        let code = r#"package main

func NewServer(port int) *Server {
    return &Server{port: port}
}

func (s *Server) Start() error {
    return nil
}"#;
        let (nodes, _edges) = extract(code, "go", "server.go");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"NewServer"));
        assert!(names.contains(&"Start"));
    }

    fn call_targets(edges: &[AstEdge], caller: &str) -> Vec<String> {
        let mut t: Vec<String> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.source == caller)
            .map(|e| e.target.clone())
            .collect();
        t.sort();
        t
    }

    #[test]
    fn test_rust_function_passed_as_argument_is_a_call() {
        // A function used as a value is called by the function that passes it:
        // `cora callers`, `impact` and `dead-code` must see it (no parentheses, so
        // no `call_expression`).
        let code = r#"
fn helper(x: u32) -> u32 { x }
struct S;
impl S { fn make(v: u32) -> S { S } }
fn run(items: Vec<u32>) {
    let _a: Vec<u32> = items.iter().copied().map(helper).collect();
    let _b: Vec<S> = items.into_iter().map(S::make).collect();
    let _c = other(0, MAX_LEN, None, Mode::Fast, "text", 7, &items);
    let (conn, project_id) = setup();
    use_both(conn, project_id);
    if let Some(inner) = lookup() {
        take(inner);
    }
    match find() {
        Ok(value) => keep(value),
        Err(_) => {}
    }
    for (index, entry) in items.iter().enumerate() {
        visit(index, entry);
    }
    match command {
        Command::Callers { symbol, limit } => callers(symbol, limit),
        _ => {}
    }
}
"#;
        let (_nodes, edges) = extract(code, "rs", "run.rs");
        let targets = call_targets(&edges, "run");
        assert!(targets.contains(&"helper".to_string()), "{targets:?}");
        assert!(targets.contains(&"make".to_string()), "{targets:?}");
        let make = edges.iter().find(|e| e.target == "make").unwrap();
        assert_eq!(make.qualifier.as_deref(), Some("S"));
        for noise in [
            "MAX_LEN",
            "None",
            "Fast",
            "Mode",
            "text",
            "items",
            "conn",
            "project_id",
            "inner",
            "value",
            "index",
            "entry",
            "symbol",
            "limit",
        ] {
            assert!(
                !targets.contains(&noise.to_string()),
                "{noise} is not a function: {targets:?}"
            );
        }
    }

    #[test]
    fn test_rust_serde_attribute_functions_are_calls_of_the_struct() {
        let code = r#"
#[derive(Deserialize)]
struct Cfg {
    #[serde(default = "default_true")]
    on: bool,
    #[serde(skip_serializing_if = "is_default", deserialize_with = "crate::de::lenient")]
    n: u32,
    /// default = "not_an_attribute"
    note: String,
}
fn default_true() -> bool { true }
"#;
        let (_nodes, edges) = extract(code, "rs", "cfg.rs");
        let targets = call_targets(&edges, "Cfg");
        for want in ["default_true", "is_default", "lenient"] {
            assert!(
                targets.contains(&want.to_string()),
                "missing {want}: {targets:?}"
            );
        }
        assert!(
            !targets.contains(&"not_an_attribute".to_string()),
            "{targets:?}"
        );
        let lenient = edges.iter().find(|e| e.target == "lenient").unwrap();
        assert_eq!(lenient.qualifier.as_deref(), Some("de"));
    }

    #[test]
    fn test_extract_ruby_module_methods() {
        // #791: the methods of a Ruby module were never indexed because the
        // first `"module"` arm shadowed the one that extracts them.
        let code =
            "module Greeter\n  def hello\n    puts 'hi'\n  end\n\n  def goodbye\n  end\nend\n";
        let (nodes, _edges) = extract(code, "rb", "greeter.rb");
        let modules: Vec<&str> = nodes
            .iter()
            .filter(|n| matches!(n.kind, SymbolKind::Module))
            .map(|n| n.name.as_str())
            .collect();
        let methods: Vec<(&str, Option<&str>)> = nodes
            .iter()
            .filter(|n| matches!(n.kind, SymbolKind::Method))
            .map(|n| (n.name.as_str(), n.parent.as_deref()))
            .collect();
        assert_eq!(
            modules,
            vec!["Greeter"],
            "the module itself is indexed once"
        );
        assert!(
            methods.contains(&("hello", Some("Greeter"))),
            "got {methods:?}"
        );
        assert!(
            methods.contains(&("goodbye", Some("Greeter"))),
            "got {methods:?}"
        );
    }

    #[test]
    fn test_extract_ruby_nested_classes_and_modules() {
        // #791: `module Outer; class Inner; def run` is the usual Ruby layout, but only the
        // direct children of the file were visited, so nothing inside Outer was indexed.
        let code = "module Outer\n  VERSION = 1\n\n  module Deep\n    class Leaf\n      def leaf_method\n      end\n    end\n  end\n\n  class Inner < Base\n    def run\n      helper\n    end\n  end\nend\n";
        let (nodes, edges) = extract(code, "rb", "outer.rb");
        let kinds = |k: SymbolKind| -> Vec<&str> {
            nodes
                .iter()
                .filter(|n| n.kind == k)
                .map(|n| n.name.as_str())
                .collect()
        };
        let modules = kinds(SymbolKind::Module);
        let classes = kinds(SymbolKind::Class);
        let methods = kinds(SymbolKind::Method);
        assert!(
            modules.contains(&"Outer") && modules.contains(&"Deep"),
            "{modules:?}"
        );
        assert!(
            classes.contains(&"Inner") && classes.contains(&"Leaf"),
            "{classes:?}"
        );
        assert!(
            methods.contains(&"run") && methods.contains(&"leaf_method"),
            "{methods:?}"
        );
        assert_eq!(
            nodes.iter().filter(|n| n.name == "Outer").count(),
            1,
            "a nested class or module must not be indexed twice: {nodes:?}"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.target == "Base"),
            "the superclass edge of a nested class is kept: {edges:?}"
        );
    }

    #[test]
    fn test_extract_ruby_class_methods() {
        let code = "class Account\n  def balance\n    0\n  end\nend\n";
        let (nodes, _edges) = extract(code, "rb", "account.rb");
        assert!(
            nodes
                .iter()
                .any(|n| n.name == "balance" && matches!(n.kind, SymbolKind::Method)),
            "class methods must keep being indexed: {nodes:?}"
        );
    }

    #[test]
    fn test_extract_python_class() {
        let code = r#"class AuthService:
    def __init__(self):
        self.secret = ""

    async def validate(self, token: str) -> bool:
        return False

    def refresh(self):
        pass"#;
        let (nodes, _edges) = extract(code, "py", "auth.py");
        let classes: Vec<&str> = nodes
            .iter()
            .filter(|n| matches!(n.kind, SymbolKind::Class))
            .map(|n| n.name.as_str())
            .collect();
        let methods: Vec<&str> = nodes
            .iter()
            .filter(|n| matches!(n.kind, SymbolKind::Method))
            .map(|n| n.name.as_str())
            .collect();
        assert!(classes.contains(&"AuthService"));
        assert!(methods.contains(&"validate"));
        assert!(methods.contains(&"refresh"));
    }

    #[test]
    fn test_extract_typescript_interface() {
        let code = r#"export interface User {
    id: string;
    name: string;
}

export const DEFAULT_TIMEOUT = 5000;

export function getUser(id: string): User {
    return null as any;
}"#;
        let (nodes, _edges) = extract(code, "ts", "user.ts");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"User"));
        assert!(names.contains(&"DEFAULT_TIMEOUT"));
        assert!(names.contains(&"getUser"));
    }

    #[test]
    fn test_extract_unsupported_fallback() {
        // "rb" is now supported; use a truly unsupported language
        let (nodes, edges) = extract("fn test() {}", "xyz", "test.xyz");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn test_extract_empty() {
        let (nodes, edges) = extract("", "rs", "empty.rs");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    // ─── Svelte + TS arrow function tests (#384) ───────────────────

    #[test]
    fn test_extract_ts_arrow_function() {
        let code = r#"export const handler = () => {
    return 42;
};

export const processForm = (data: string) => {
    handler();
    return data.toUpperCase();
};"#;
        let (nodes, edges) = extract(code, "ts", "handler.ts");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"handler"),
            "arrow function 'handler' should be extracted, got: {:?}",
            names
        );
        assert!(
            names.contains(&"processForm"),
            "arrow function 'processForm' should be extracted, got: {:?}",
            names
        );
        // Verify call edge: processForm calls handler
        assert!(edges.iter().any(|e| e.source == "processForm"
            && e.target == "handler"
            && e.kind == EdgeKind::Calls));
    }

    #[test]
    fn test_extract_ts_function_expression() {
        let code = r#"const callback = function doStuff() {
    console.log("hi");
};"#;
        let (nodes, _edges) = extract(code, "ts", "callback.ts");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        // function_expression should be captured
        assert!(
            names.contains(&"callback"),
            "function expression 'callback' should be extracted, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_svelte_basic() {
        let code = r#"<script lang="ts">
    import { onMount } from 'svelte';

    export let name: string;

    function greet() {
        return `Hello ${name}`;
    }

    export const handler = () => {
        greet();
    };

    onMount(() => {
        greet();
    });
</script>

<h1>{greet()}</h1>"#;
        let (nodes, edges) = extract(code, "svelte", "Hello.svelte");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"greet"),
            "function 'greet' should be extracted from Svelte script, got: {:?}",
            names
        );
        assert!(
            names.contains(&"handler"),
            "arrow function 'handler' should be extracted from Svelte script, got: {:?}",
            names
        );
        // Verify call edge: handler calls greet
        assert!(
            edges
                .iter()
                .any(|e| e.source == "handler" && e.target == "greet" && e.kind == EdgeKind::Calls)
        );
    }

    fn call_edges<'a>(edges: &'a [AstEdge], callee: &str) -> Vec<&'a AstEdge> {
        edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.target == callee)
            .collect()
    }

    /// #714: `Type::method()` records the type as the call's qualifier while the
    /// target stays the bare method name.
    #[test]
    fn a_type_path_call_records_the_type_as_qualifier() {
        let code = r#"
pub fn roster() {
    let _a = WebSearchTool::from_env();
    let _b = crate::web::WebFetchTool::<u8>::from_env();
    let _c = helper();
}
"#;
        let (_, edges) = extract(code, "rs", "roster.rs");
        let from_env = call_edges(&edges, "from_env");
        assert_eq!(from_env.len(), 2);
        assert_eq!(from_env[0].qualifier.as_deref(), Some("WebSearchTool"));
        assert_eq!(from_env[1].qualifier.as_deref(), Some("WebFetchTool"));
        assert_eq!(call_edges(&edges, "helper")[0].qualifier, None);
    }

    /// #714: `self.m()` and `Self::m()` resolve to the enclosing impl type; a
    /// method call on any other value stays ambiguous (no qualifier).
    #[test]
    fn self_calls_resolve_to_the_impl_type_and_other_receivers_stay_ambiguous() {
        let code = r#"
struct A;
impl A {
    fn run(&self, other: &B) {
        self.step();
        Self::make();
        other.step();
    }
}
"#;
        let (_, edges) = extract(code, "rs", "a.rs");
        let step = call_edges(&edges, "step");
        assert_eq!(step.len(), 2);
        assert_eq!(step[0].qualifier.as_deref(), Some("A"));
        assert_eq!(step[1].qualifier, None);
        assert_eq!(
            call_edges(&edges, "make")[0].qualifier.as_deref(),
            Some("A")
        );
        assert!(step.iter().all(|e| e.caller_type.as_deref() == Some("A")));
    }

    /// #714: a path call inside macro arguments keeps its type too.
    #[test]
    fn a_type_path_call_in_a_macro_keeps_its_qualifier() {
        let code = r#"
fn f() {
    let _v = vec![Widget::build(1)];
}
"#;
        let (_, edges) = extract(code, "rs", "m.rs");
        assert_eq!(
            call_edges(&edges, "build")[0].qualifier.as_deref(),
            Some("Widget")
        );
    }

    /// Regression (#519): method calls (`self.export_full()`,
    /// `manager.remember_with_contradiction()`) and qualified paths
    /// (`std::mem::drop(...)`) must record the FINAL name segment as the
    /// call target, so the edge joins against symbol names across files and
    /// crates. Previously the raw text (`self.export_full`) was stored and
    /// dead-code mis-flagged these symbols as uncalled.
    #[test]
    fn test_method_call_target_records_bare_name() {
        let code = r#"
pub struct StructuralExporter;

impl StructuralExporter {
    pub fn export_full(&self) -> String { String::new() }
}

struct Manager;

impl Manager {
    pub fn maintenance(&self, x: &StructuralExporter) {
        let out = x.export_full();
        std::mem::drop(out);
    }
}
"#;
        let (nodes, edges) = extract(code, "rs", "maintenance.rs");
        assert!(nodes.iter().any(|n| n.name == "export_full"));

        // Method-call edge lands on the bare method name…
        assert!(
            edges.iter().any(|e| e.source == "maintenance"
                && e.target == "export_full"
                && e.kind == EdgeKind::Calls),
            "expected Calls edge to bare name 'export_full', got: {:?}",
            edges
                .iter()
                .filter(|e| e.kind == EdgeKind::Calls)
                .map(|e| &e.target)
                .collect::<Vec<_>>()
        );

        // …and qualified paths resolve to their final segment too.
        assert!(
            edges
                .iter()
                .any(|e| e.source == "maintenance" && e.target == "drop"),
            "expected Calls edge to 'drop' from std::mem::drop"
        );
    }

    #[test]
    fn test_extract_svelte_line_numbers() {
        let code = r#"<script lang="ts">
    function foo() {}
    function bar() { foo(); }
</script>"#;
        let (nodes, _edges) = extract(code, "svelte", "test.svelte");
        let foo_node = nodes
            .iter()
            .find(|n| n.name == "foo")
            .expect("foo not found");
        let bar_node = nodes
            .iter()
            .find(|n| n.name == "bar")
            .expect("bar not found");
        // foo is on line 2 (1-based), bar on line 3
        assert_eq!(
            foo_node.line, 2,
            "foo should be on line 2, got {}",
            foo_node.line
        );
        assert_eq!(
            bar_node.line, 3,
            "bar should be on line 3, got {}",
            bar_node.line
        );
    }

    #[test]
    fn test_extract_svelte_no_script() {
        // Svelte file with no script block — should return empty
        let code = r#"<div>
    <h1>Hello</h1>
</div>"#;
        let (nodes, edges) = extract(code, "svelte", "noscript.svelte");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn test_extract_svelte_javascript_lang() {
        let code = r#"<script lang="js">
    function plainJs() {
        return 42;
    }
</script>"#;
        let (nodes, _edges) = extract(code, "svelte", "plain.svelte");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"plainJs"),
            "JS function in Svelte should be extracted, got: {:?}",
            names
        );
    }

    #[test]
    fn test_extract_svelte_inline_script() {
        // Single-line script block
        let code = r#"<script>function inline() { return 1; }</script>
<div />"#;
        let (nodes, _edges) = extract(code, "svelte", "inline.svelte");
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"inline"),
            "inline script function should be extracted, got: {:?}",
            names
        );
    }

    // ─── Edge extraction tests (#437) ───────────────────────────────

    #[test]
    fn test_extract_typescript_class_extends() {
        let code = r#"class Dog extends Animal {
    bark() { return; }
}"#;
        let (_nodes, edges) = extract(code, "ts", "dog.ts");
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.source == "Dog" && e.target == "Animal"),
            "expected Dog -> Animal Inherits edge, got edges: {:?}",
            edges
        );
    }

    #[test]
    fn test_extract_typescript_class_implements() {
        let code = r#"class Repository implements Comparable, Serializable {
    compare() { return 0; }
}"#;
        let (_nodes, edges) = extract(code, "ts", "repo.ts");
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::Implements
                && e.source == "Repository"
                && e.target == "Comparable"),
            "expected Repository -> Comparable Implements edge, got: {:?}",
            edges
        );
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::Implements
                && e.source == "Repository"
                && e.target == "Serializable"),
            "expected Repository -> Serializable Implements edge, got: {:?}",
            edges
        );
    }

    #[test]
    fn test_extract_php_class_extends() {
        let code = r#"<?php
class Dog extends Animal {
    public function bark() { return; }
}"#;
        let (_nodes, edges) = extract(code, "php", "dog.php");
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.source == "Dog" && e.target == "Animal"),
            "expected Dog -> Animal Inherits edge, got edges: {:?}",
            edges
        );
    }

    #[test]
    fn test_extract_php_class_implements() {
        let code = r#"<?php
class Repository implements Comparable, Serializable {
    public function compare() { return 0; }
}"#;
        let (_nodes, edges) = extract(code, "php", "repo.php");
        assert!(
            edges.iter().any(|e| e.kind == EdgeKind::Implements
                && e.source == "Repository"
                && e.target == "Comparable"),
            "expected Repository -> Comparable Implements edge, got: {:?}",
            edges
        );
    }

    #[test]
    fn test_extract_scala_class_extends() {
        let code = "class Dog extends Animal {\n  def bark(): Unit = {}\n}";
        let (_nodes, edges) = extract(code, "scala", "dog.scala");
        assert!(
            edges
                .iter()
                .any(|e| e.kind == EdgeKind::Inherits && e.source == "Dog" && e.target == "Animal"),
            "expected Dog -> Animal Inherits edge, got edges: {:?}",
            edges
        );
    }

    // ─── #715: calls inside macro arguments ───

    const MACRO_CODE: &str = r#"
fn tool_kind(t: &str) -> &str { t }
fn approval_config_option(a: bool) -> u8 { 1 }

fn build(tool: &str, auto: bool, x: Thing) {
    let v = json!({ "kind": tool_kind(tool), "n": 1 });
    let o = vec![approval_config_option(auto)];
    let s = format!("{}", helper_a(tool));
    assert!(helper_b(1) == 2, "msg {}", helper_c());
    let m = matches!(helper_d(), Some(_));
    println!("{}", self::util::helper_e(tool));
    dbg!(x.method_f(1));
    plain_call(tool);
}

macro_rules! gen {
    () => { fn declared_not_called() {} };
}
"#;

    fn macro_targets() -> Vec<String> {
        let (_, edges) = extract(MACRO_CODE, "rs", "m.rs");
        edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.source == "build")
            .map(|e| e.target.clone())
            .collect()
    }

    #[test]
    fn calls_inside_std_and_serde_macros_are_call_edges() {
        let targets = macro_targets();
        for want in [
            "tool_kind",
            "approval_config_option",
            "helper_a",
            "helper_b",
            "helper_c",
            "helper_d",
            "helper_e",
            "method_f",
            "plain_call",
        ] {
            assert!(
                targets.iter().any(|t| t == want),
                "{want} missing: {targets:?}"
            );
        }
    }

    #[test]
    fn the_macro_names_and_declarations_are_not_calls() {
        let (_, edges) = extract(MACRO_CODE, "rs", "m.rs");
        let all: Vec<&str> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .map(|e| e.target.as_str())
            .collect();
        for not in [
            "json", "vec", "format", "assert", "matches", "println", "dbg",
        ] {
            assert!(!all.contains(&not), "macro `{not}` is not a call: {all:?}");
        }
        assert!(
            !all.contains(&"declared_not_called"),
            "`fn name()` inside macro_rules! is a definition: {all:?}"
        );
    }

    #[test]
    fn a_call_in_a_macro_is_recorded_once_with_its_own_line() {
        let targets = macro_targets();
        assert_eq!(targets.iter().filter(|t| *t == "tool_kind").count(), 1);
        let (_, edges) = extract(MACRO_CODE, "rs", "m.rs");
        let e = edges
            .iter()
            .find(|e| e.target == "tool_kind" && e.source == "build")
            .unwrap();
        let expected = MACRO_CODE
            .lines()
            .position(|l| l.contains("json!"))
            .map(|n| n as u32 + 1)
            .unwrap();
        assert_eq!(e.line, expected);
    }
}
