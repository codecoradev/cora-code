//! Edge types of the code graph, shared by the Tree-sitter extractor and the regex
//! route detector (the regex path also builds in a no-Tree-sitter build, #793).

use crate::index::extract::CallSite;

/// Edge type in the knowledge graph.
///
/// Without the `tree-sitter` feature only the regex route detector builds edges, so
/// every kind but `Route` is made by the Tree-sitter extractor alone.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(feature = "tree-sitter"), allow(dead_code))]
pub enum EdgeKind {
    Calls,
    Imports,
    Implements,
    Inherits,
    Route,
}

#[cfg_attr(not(feature = "tree-sitter"), allow(dead_code))]
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
#[cfg_attr(not(feature = "tree-sitter"), allow(dead_code))] // `kind` is read by the Tree-sitter path
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
