//! Symbol types for the index.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Kind of a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Method,
    Constant,
    Module,
    TypeAlias,
    Variable,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Class => "class",
            Self::Method => "method",
            Self::Constant => "constant",
            Self::Module => "module",
            Self::TypeAlias => "type_alias",
            Self::Variable => "variable",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "function" => Self::Function,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "interface" => Self::Interface,
            "class" => Self::Class,
            "method" => Self::Method,
            "constant" => Self::Constant,
            "module" => Self::Module,
            "type_alias" => Self::TypeAlias,
            "variable" => Self::Variable,
            _ => Self::Variable,
        }
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A symbol stored in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedSymbol {
    pub id: i64,
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: u32,
    pub signature: String,
    pub language: String,
}

/// Query parameters for searching the index.
#[derive(Debug, Clone, Default)]
pub struct SymbolQuery {
    /// FTS5 search text (matches name + signature).
    pub text: Option<String>,
    /// Filter by symbol kind.
    pub kind: Option<SymbolKind>,
    /// Filter by file path (exact or prefix).
    pub file_prefix: Option<String>,
    /// Filter by language.
    pub language: Option<String>,
    /// Maximum results.
    pub limit: usize,
}

impl SymbolQuery {
    /// Create a text search query.
    #[allow(dead_code)]
    pub fn text(text: &str) -> Self {
        Self {
            text: Some(text.to_string()),
            limit: 50,
            ..Default::default()
        }
    }

    /// Create a kind-filtered query.
    #[allow(dead_code)]
    pub fn kind(kind: SymbolKind) -> Self {
        Self {
            kind: Some(kind),
            limit: 50,
            ..Default::default()
        }
    }
}

/// A search result from the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub symbol: IndexedSymbol,
    pub score: f64,
}

/// Execute a symbol search query against the database, scoped to a project.
pub fn search(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
) -> anyhow::Result<Vec<SearchResult>> {
    let limit = if query.limit > 0 {
        query.limit as i64
    } else {
        50
    };

    if let Some(text) = &query.text {
        // FTS5 full-text search
        let fts_query = sanitize_fts_query(text);

        let mut sql = String::from(
            "SELECT s.id, s.name, s.kind, s.file, s.line, s.signature, s.language,
                    bm25(symbols_fts) as score
             FROM symbols_fts
             JOIN symbols s ON s.id = symbols_fts.rowid
             WHERE symbols_fts MATCH ?1 AND s.project_id = ?2",
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(fts_query.clone()), Box::new(project_id)];

        let mut param_idx = 3;

        if let Some(kind) = &query.kind {
            sql.push_str(&format!(" AND s.kind = ?{param_idx}"));
            params.push(Box::new(kind.as_str().to_string()));
            param_idx += 1;
        }

        if let Some(lang) = &query.language {
            sql.push_str(&format!(" AND s.language = ?{param_idx}"));
            params.push(Box::new(lang.clone()));
            param_idx += 1;
        }

        if let Some(prefix) = &query.file_prefix {
            sql.push_str(&format!(" AND s.file LIKE ?{param_idx}"));
            params.push(Box::new(format!("{prefix}%")));
            // param_idx not needed after last push
        }

        sql.push_str(&format!(" ORDER BY score ASC LIMIT {limit}"));

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(SearchResult {
                symbol: IndexedSymbol {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: SymbolKind::from_str(&row.get::<_, String>(2)?),
                    file: row.get(3)?,
                    line: row.get::<_, i64>(4)? as u32,
                    signature: row.get(5)?,
                    language: row.get(6)?,
                },
                // bm25 returns negative scores (more negative = better match)
                // Convert to positive score where higher = better
                score: {
                    let raw: f64 = row.get(7)?;
                    if raw < 0.0 { -raw } else { raw }
                },
            })
        })?;

        let mut results: Vec<SearchResult> = rows.filter_map(|r| r.ok()).collect();

        // If FTS returns nothing, try LIKE fallback on name
        if results.is_empty() {
            results = like_search(conn, project_id, text, query, limit)?;
        }

        Ok(results)
    } else {
        // No text query — just filter by kind/file/language
        filter_search(conn, project_id, query, limit)
    }
}

/// Fallback: LIKE-based search when FTS5 returns nothing.
#[allow(unused_assignments)]
fn like_search(
    conn: &Connection,
    project_id: i64,
    text: &str,
    query: &SymbolQuery,
    limit: i64,
) -> anyhow::Result<Vec<SearchResult>> {
    let pattern = format!("%{text}%");

    let mut sql = String::from(
        "SELECT id, name, kind, file, line, signature, language
         FROM symbols WHERE name LIKE ?1 AND project_id = ?2",
    );

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(pattern), Box::new(project_id)];
    let mut idx = 3;

    if let Some(kind) = &query.kind {
        sql.push_str(&format!(" AND kind = ?{idx}"));
        params.push(Box::new(kind.as_str().to_string()));
        idx += 1;
    }

    sql.push_str(&format!(" LIMIT {limit}"));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(SearchResult {
            symbol: IndexedSymbol {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: SymbolKind::from_str(&row.get::<_, String>(2)?),
                file: row.get(3)?,
                line: row.get::<_, i64>(4)? as u32,
                signature: row.get(5)?,
                language: row.get(6)?,
            },
            score: 1.0,
        })
    })?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Filter-only search (no FTS text).
#[allow(unused_assignments)]
fn filter_search(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
    limit: i64,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut sql = String::from(
        "SELECT id, name, kind, file, line, signature, language FROM symbols WHERE project_id = ?1",
    );

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];
    let mut idx = 2;

    if let Some(kind) = &query.kind {
        sql.push_str(&format!(" AND kind = ?{idx}"));
        params.push(Box::new(kind.as_str().to_string()));
        idx += 1;
    }

    if let Some(lang) = &query.language {
        sql.push_str(&format!(" AND language = ?{idx}"));
        params.push(Box::new(lang.clone()));
        idx += 1;
    }

    if let Some(prefix) = &query.file_prefix {
        sql.push_str(&format!(" AND file LIKE ?{idx}"));
        params.push(Box::new(format!("{prefix}%")));
        idx += 1;
    }

    sql.push_str(&format!(" LIMIT {limit}"));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(SearchResult {
            symbol: IndexedSymbol {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: SymbolKind::from_str(&row.get::<_, String>(2)?),
                file: row.get(3)?,
                line: row.get::<_, i64>(4)? as u32,
                signature: row.get(5)?,
                language: row.get(6)?,
            },
            score: 0.0,
        })
    })?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Sanitize a user query for FTS5 MATCH syntax.
///
/// Enhances search quality by:
/// - Splitting camelCase/PascalCase identifiers into sub-words
///   (e.g. "camelCase" → "camel" OR "Case")
/// - Lowercasing tokens (unicode61 is case-insensitive, but explicit is safer)
/// - Quoting tokens to prevent FTS5 syntax errors
///
/// Output format: "camelCase" OR "camel" OR "Case" "otherToken"
fn sanitize_fts_query(text: &str) -> String {
    let tokens: Vec<String> = text
        .split_whitespace()
        .map(|token| {
            let clean: String = token
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            clean
        })
        .filter(|s| !s.is_empty())
        .collect();

    let mut parts = Vec::new();

    for token in &tokens {
        // Try camelCase/PascalCase split
        let subwords = split_camel_case(token);

        if subwords.len() > 1 {
            // Original + subwords joined with OR for broader matching
            let expanded: Vec<String> = subwords.iter().map(|w| format!("\"{w}\"")).collect();
            // Also include the original token for exact matching
            let all: Vec<String> = std::iter::once(format!("\"{token}\""))
                .chain(expanded)
                .collect();
            parts.push(all.join(" OR "));
        } else {
            parts.push(format!("\"{token}\""));
        }
    }

    parts.join(" ")
}

/// Split a camelCase or PascalCase identifier into sub-words.
///
/// "camelCase" → ["camel", "Case"]
/// "PascalCase" → ["Pascal", "Case"]
/// "XMLParser" → ["XML", "Parser"]
/// "simple" → ["simple"]
/// "snake_case" → ["snake_case"] (no split)
/// "HTTPResponse" → ["HTTP", "Response"]
fn split_camel_case(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = s.chars().collect();

    if chars.is_empty() {
        return words;
    }

    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            // Underscore boundary — keep as part of current word
            current.push(c);
            continue;
        }

        if c.is_uppercase() {
            // Check if we should split before this uppercase
            let prev_is_lower = current
                .chars()
                .last()
                .map(|p| p.is_lowercase())
                .unwrap_or(false);
            let next_is_lower = chars.get(i + 1).map(|n| n.is_lowercase()).unwrap_or(false);

            if prev_is_lower || (next_is_lower && !current.is_empty()) {
                // Split: lowercase→uppercase or "A" in "XMLParser"
                if !current.is_empty() {
                    words.push(current.clone());
                    current.clear();
                }
            }
        }
        current.push(c);
    }

    if !current.is_empty() {
        words.push(current);
    }

    // If no split happened, return the original
    if words.len() <= 1 {
        vec![s.to_string()]
    } else {
        words
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_kind_round_trip() {
        let kinds = [
            SymbolKind::Function,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Trait,
            SymbolKind::Method,
            SymbolKind::Constant,
        ];
        for kind in &kinds {
            let s = kind.as_str();
            assert_eq!(SymbolKind::from_str(s), *kind);
        }
    }

    #[test]
    fn test_symbol_kind_display() {
        assert_eq!(SymbolKind::Function.to_string(), "function");
        assert_eq!(SymbolKind::Struct.to_string(), "struct");
    }

    #[test]
    fn test_sanitize_fts_query() {
        assert_eq!(sanitize_fts_query("auth"), "\"auth\"");
        assert_eq!(sanitize_fts_query("auth login"), "\"auth\" \"login\"");
        assert_eq!(
            sanitize_fts_query("auth; DROP TABLE"),
            "\"auth\" \"DROP\" \"TABLE\""
        );
        assert_eq!(sanitize_fts_query(""), "");
        // camelCase expansion
        let q = sanitize_fts_query("camelCase");
        assert!(q.contains("\"camelCase\""));
        assert!(q.contains("\"camel\""));
        assert!(q.contains("\"Case\""));
        assert!(q.contains(" OR "));
        // PascalCase
        let q = sanitize_fts_query("PascalCase");
        assert!(q.contains("\"PascalCase\""));
        assert!(q.contains("\"Pascal\""));
        assert!(q.contains("\"Case\""));
        // All caps (no split)
        assert_eq!(sanitize_fts_query("HTTP"), "\"HTTP\"");
        // Mixed: camelCase + plain token
        let q = sanitize_fts_query("camelCase helper");
        assert!(q.contains("\"camel\""));
        assert!(q.contains("\"helper\""));
    }

    #[test]
    fn test_symbol_query_text() {
        let q = SymbolQuery::text("authenticate");
        assert_eq!(q.text.as_deref(), Some("authenticate"));
        assert_eq!(q.limit, 50);
    }

    #[test]
    fn test_symbol_query_kind() {
        let q = SymbolQuery::kind(SymbolKind::Function);
        assert_eq!(q.kind, Some(SymbolKind::Function));
        assert_eq!(q.limit, 50);
    }

    #[test]
    fn test_split_camel_case() {
        use super::split_camel_case;

        assert_eq!(split_camel_case("camelCase"), vec!["camel", "Case"]);
        assert_eq!(split_camel_case("PascalCase"), vec!["Pascal", "Case"]);
        assert_eq!(split_camel_case("XMLParser"), vec!["XML", "Parser"]);
        assert_eq!(split_camel_case("simple"), vec!["simple"]);
        assert_eq!(split_camel_case("snake_case"), vec!["snake_case"]);
        assert_eq!(split_camel_case("HTTPResponse"), vec!["HTTP", "Response"]);
        assert_eq!(split_camel_case("getURL"), vec!["get", "URL"]);
        assert_eq!(split_camel_case("a"), vec!["a"]);
        assert_eq!(split_camel_case(""), Vec::<String>::new());
        assert_eq!(split_camel_case("ABTest"), vec!["AB", "Test"]);
    }
}
