// ! Tree-sitter AST extraction.
// !
// ! Provides proper AST-based symbol and edge extraction using tree-sitter.
// ! Only compiled when the `tree-sitter` feature is enabled.

#![cfg(feature = "tree-sitter")]
#![allow(dead_code, unused)]

use crate::index::extract::CallSite;
use crate::index::extract::ExtractedDef;
use crate::index::symbols::SymbolKind;

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
    ChildOf,
    Route,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "CALLS",
            Self::Imports => "IMPORTS",
            Self::Implements => "IMPLEMENTS",
            Self::Inherits => "INHERITS",
            Self::ChildOf => "CHILD_OF",
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
}

impl From<&AstEdge> for CallSite {
    fn from(e: &AstEdge) -> Self {
        Self {
            caller: e.source.clone(),
            callee: e.target.clone(),
            file: e.file.clone(),
            line: e.line,
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
                                                name,
                                                kind: SymbolKind::Function,
                                                file: file_path.to_string(),
                                                line: (gc.start_position().row + 1) as u32,
                                                signature: signature_for_node(&gc, source),
                                                parent: Some(type_n.clone()),
                                            });
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
                    });
                }
            }
            _ => {}
        }
    }
    (nodes, edges)
}

/// Walk function body for call expressions using cursor-based DFS.
fn extract_calls_from_node(
    node: &tree_sitter::Node,
    source: &str,
    file_path: &str,
    caller: &str,
    edges: &mut Vec<AstEdge>,
) {
    fn walk_calls(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        caller: &str,
        edges: &mut Vec<AstEdge>,
    ) {
        if node.kind() == "call_expression" {
            if let Some(fn_node) = node.child_by_field_name("function") {
                let callee = node_text(&fn_node, source);
                if !callee.is_empty() {
                    edges.push(AstEdge {
                        source: caller.to_string(),
                        kind: EdgeKind::Calls,
                        target: callee,
                        file: file_path.to_string(),
                        line: (node.start_position().row + 1) as u32,
                    });
                }
            }
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                walk_calls(&cursor.node(), source, file_path, caller, edges);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    walk_calls(node, source, file_path, caller, edges);
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
            "declaration" => {
                // Top-level const/typedef etc
                if let Some(decl) = child.child_by_field_name("declarator") {
                    // Only capture if it looks like a function pointer or named constant
                    let mut dc = child.walk();
                    if dc.goto_first_child() {
                        loop {
                            if dc.node().kind() == "type_qualifier" {
                                // const — skip for now (too noisy)
                            }
                            if !dc.goto_next_sibling() {
                                break;
                            }
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
                                    let edge_kind = if kind == SymbolKind::Interface {
                                        EdgeKind::Inherits
                                    } else {
                                        EdgeKind::Implements
                                    };
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

    fn walk_ruby(
        node: &tree_sitter::Node,
        source: &str,
        file_path: &str,
        mut nodes: &mut Vec<AstNode>,
        edges: &mut Vec<AstEdge>,
    ) {
        match node.kind() {
            "module" => {
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
                        let parent = node_text(&superclass, source);
                        if !parent.is_empty() {
                            edges.push(AstEdge {
                                source: name.clone(),
                                kind: EdgeKind::Inherits,
                                target: parent,
                                file: file_path.to_string(),
                                line,
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
}
