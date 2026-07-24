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
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Calls => "CALLS",
            Self::Imports => "IMPORTS",
            Self::Implements => "IMPLEMENTS",
            Self::Inherits => "INHERITS",
            Self::ChildOf => "CHILD_OF",
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
    if let Some(n) =
        find_child_by_kind(node, &["identifier", "type_identifier", "field_identifier"])
    {
        return node_text(&n, source);
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
                // const/let declarations
                let mut vc = node.walk();
                if vc.goto_first_child() {
                    loop {
                        if vc.node().kind() == "variable_declarator" {
                            let name = node_name(&vc.node(), source);
                            // Only extract as constant if ALL_CAPS
                            if !name.is_empty() && name == name.to_uppercase() && name.len() > 1 {
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
            "variable_declaration" => {
                let mut vc = node.walk();
                if vc.goto_first_child() {
                    loop {
                        if vc.node().kind() == "variable_declarator" {
                            let name = node_name(&vc.node(), source);
                            if !name.is_empty() && name == name.to_uppercase() && name.len() > 1 {
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
                                            .map_or(false, |c| c.is_ascii_uppercase())
                                    {
                                        EdgeKind::Implements
                                    } else if kind == SymbolKind::Interface {
                                        EdgeKind::Inherits
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
        nodes: &mut Vec<AstNode>,
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
                    // Methods inside class
                    let mut mc = node.walk();
                    if mc.goto_first_child() {
                        loop {
                            if mc.node().kind() == "method" {
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
}
