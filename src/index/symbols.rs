//! Symbol types for the index.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Every symbol kind, in display order.
pub const ALL_KINDS: [SymbolKind; 11] = [
    SymbolKind::Function,
    SymbolKind::Struct,
    SymbolKind::Enum,
    SymbolKind::Trait,
    SymbolKind::Interface,
    SymbolKind::Class,
    SymbolKind::Method,
    SymbolKind::Constant,
    SymbolKind::Module,
    SymbolKind::TypeAlias,
    SymbolKind::Variable,
];

impl SymbolKind {
    /// Comma-separated list of valid `--kind` values, for error messages.
    pub fn valid_list() -> String {
        ALL_KINDS
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

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

    /// Strict parse of a user-supplied kind (`type-alias` and `type_alias` both
    /// work); `None` for an unknown kind.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase().replace('-', "_");
        ALL_KINDS.iter().find(|k| k.as_str() == s).cloned()
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
    /// Also match words in the signature / doc text (default: name only).
    pub include_docs: bool,
}

impl SymbolQuery {
    /// Create a text search query.
    #[allow(dead_code)]
    pub fn text(text: &str) -> Self {
        Self {
            text: Some(text.to_string()),
            limit: 50,
            // Programmatic callers (brain, MCP bridge) keep matching signatures.
            include_docs: true,
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

/// Result of a symbol search: the (possibly truncated) ranked results plus the
/// number of matches before the limit was applied.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    /// Total matches before `limit` was applied (`>= results.len()`).
    pub total: usize,
}

/// Execute a symbol search query against the database, scoped to a project.
pub fn search(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
) -> anyhow::Result<Vec<SearchResult>> {
    Ok(search_with_total(conn, project_id, query)?.results)
}

/// Match tier of `name` against the search `text`, lower is better:
/// 0 exact, 1 prefix, 2 substring, 3 anything else (FTS-only match).
/// Comparison is case-insensitive.
pub fn match_tier(name: &str, text: &str) -> u8 {
    let name = name.to_lowercase();
    let text = text.to_lowercase();
    if name == text {
        0
    } else if name.starts_with(&text) {
        1
    } else if name.contains(&text) {
        2
    } else {
        3
    }
}

/// Append the kind / language / file filters of `query` (columns prefixed with
/// `alias`) to `sql`, numbering bind parameters from `params.len() + 1`.
fn push_filters(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::ToSql>>,
    query: &SymbolQuery,
    alias: &str,
) {
    if let Some(kind) = &query.kind {
        params.push(Box::new(kind.as_str().to_string()));
        sql.push_str(&format!(" AND {alias}kind = ?{}", params.len()));
    }
    if let Some(lang) = &query.language {
        params.push(Box::new(lang.clone()));
        sql.push_str(&format!(" AND {alias}language = ?{}", params.len()));
    }
    if let Some(prefix) = &query.file_prefix {
        params.push(Box::new(format!(
            "{}%",
            super::queries::escape_like(prefix)
        )));
        sql.push_str(&format!(
            " AND {alias}file LIKE ?{} ESCAPE '\\'",
            params.len()
        ));
    }
}

fn map_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedSymbol> {
    Ok(IndexedSymbol {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: SymbolKind::from_str(&row.get::<_, String>(2)?),
        file: row.get(3)?,
        line: row.get::<_, i64>(4)? as u32,
        signature: row.get(5)?,
        language: row.get(6)?,
    })
}

/// Like [`search`], but also reports how many matches existed before `limit`.
///
/// With a text query, candidates are the union of FTS5 matches (name only,
/// or name + signature when `include_docs`) and `LIKE %text%` matches on the
/// name. They are ordered exact name > prefix > substring > FTS rank, so a
/// symbol named exactly like the query always comes first.
pub fn search_with_total(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
) -> anyhow::Result<SearchOutcome> {
    let limit = if query.limit > 0 { query.limit } else { 50 };

    let text = query.text.as_deref().map(str::trim).unwrap_or("");
    if text.is_empty() {
        return filter_search(conn, project_id, query, limit);
    }

    // id -> (symbol, raw bm25; 0.0 when only the LIKE pass found it).
    let mut candidates: HashMap<i64, (IndexedSymbol, f64)> = HashMap::new();

    // Pass 1: FTS5 token matches.
    let fts_query = sanitize_fts_query(text);
    if !fts_query.is_empty() {
        // Without `include_docs`, restrict the match to the `name` column so
        // words that only occur in the signature / doc text do not match.
        let match_expr = if query.include_docs {
            fts_query
        } else {
            format!("name : ({fts_query})")
        };
        let mut sql = String::from(
            "SELECT s.id, s.name, s.kind, s.file, s.line, s.signature, s.language,
                    bm25(symbols_fts)
             FROM symbols_fts
             JOIN symbols s ON s.id = symbols_fts.rowid
             WHERE symbols_fts MATCH ?1 AND s.project_id = ?2",
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(match_expr), Box::new(project_id)];
        push_filters(&mut sql, &mut params, query, "s.");

        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok((map_symbol(row)?, row.get::<_, f64>(7)?))
        })?;
        for (sym, raw) in rows.flatten() {
            candidates.insert(sym.id, (sym, raw));
        }
    }

    // Pass 2: substring matches on the name (FTS only matches whole tokens).
    let mut sql = String::from(
        "SELECT s.id, s.name, s.kind, s.file, s.line, s.signature, s.language
         FROM symbols s WHERE s.name LIKE ?1 ESCAPE '\\' AND s.project_id = ?2",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(format!("%{}%", super::queries::escape_like(text))),
        Box::new(project_id),
    ];
    push_filters(&mut sql, &mut params, query, "s.");
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), map_symbol)?;
    for sym in rows.flatten() {
        candidates.entry(sym.id).or_insert((sym, 0.0));
    }

    let mut ranked: Vec<(u8, f64, IndexedSymbol)> = candidates
        .into_values()
        .map(|(sym, raw)| (match_tier(&sym.name, text), raw, sym))
        .collect();
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.name.len().cmp(&b.2.name.len()))
            .then_with(|| a.2.name.cmp(&b.2.name))
            .then_with(|| a.2.file.cmp(&b.2.file))
            .then(a.2.line.cmp(&b.2.line))
    });

    let total = ranked.len();
    // bm25 is negative (more negative = better); expose it as a positive score.
    let results = ranked
        .into_iter()
        .take(limit)
        .map(|(_, raw, symbol)| SearchResult {
            symbol,
            score: raw.abs(),
        })
        .collect();
    Ok(SearchOutcome { results, total })
}

/// Filter-only search (no text).
fn filter_search(
    conn: &Connection,
    project_id: i64,
    query: &SymbolQuery,
    limit: usize,
) -> anyhow::Result<SearchOutcome> {
    let mut where_sql = String::from(" WHERE s.project_id = ?1");
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id)];
    push_filters(&mut where_sql, &mut params, query, "s.");
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM symbols s{where_sql}"),
        refs.as_slice(),
        |row| row.get(0),
    )?;

    let sql = format!(
        "SELECT s.id, s.name, s.kind, s.file, s.line, s.signature, s.language
         FROM symbols s{where_sql} ORDER BY s.name, s.file, s.line LIMIT {limit}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(refs.as_slice(), |row| {
        Ok(SearchResult {
            symbol: map_symbol(row)?,
            score: 0.0,
        })
    })?;

    Ok(SearchOutcome {
        results: rows.filter_map(|r| r.ok()).collect(),
        total: total as usize,
    })
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

    fn mem_conn() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/rank-test").unwrap();
        (conn, pid)
    }

    fn add_sym(conn: &Connection, pid: i64, name: &str, kind: &str, sig: &str) {
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, signature, language, project_id)
             VALUES (?1, ?2, 'src/a.rs', 1, ?3, 'rust', ?4)",
            rusqlite::params![name, kind, sig, pid],
        )
        .unwrap();
    }

    fn names(out: &SearchOutcome) -> Vec<&str> {
        out.results.iter().map(|r| r.symbol.name.as_str()).collect()
    }

    #[test]
    fn test_match_tier() {
        assert_eq!(match_tier("Search", "search"), 0);
        assert_eq!(match_tier("search_symbols", "search"), 1);
        assert_eq!(match_tier("brain_search", "search"), 2);
        assert_eq!(match_tier("lookup", "search"), 3);
    }

    #[test]
    fn test_ranking_exact_prefix_substring_fts() {
        let (conn, pid) = mem_conn();
        // Inserted in the reverse of the expected order.
        add_sym(&conn, pid, "brain_search", "function", "fn brain_search()");
        add_sym(
            &conn,
            pid,
            "search_symbols",
            "function",
            "fn search_symbols()",
        );
        add_sym(&conn, pid, "search", "function", "fn search()");
        add_sym(
            &conn,
            pid,
            "unrelated",
            "function",
            "fn unrelated() // search",
        );

        let mut q = SymbolQuery::text("search");
        q.include_docs = true;
        let out = search_with_total(&conn, pid, &q).unwrap();
        assert_eq!(
            names(&out),
            vec!["search", "search_symbols", "brain_search", "unrelated"]
        );
        assert_eq!(out.total, 4);
    }

    #[test]
    fn test_ranking_substring_without_token_boundary() {
        let (conn, pid) = mem_conn();
        add_sym(&conn, pid, "reindexAll", "function", "");
        add_sym(&conn, pid, "index", "function", "");
        let q = SymbolQuery {
            text: Some("index".into()),
            limit: 10,
            ..Default::default()
        };
        let out = search_with_total(&conn, pid, &q).unwrap();
        // `reindexAll` is only reachable through the substring pass.
        assert_eq!(names(&out), vec!["index", "reindexAll"]);
    }

    #[test]
    fn test_truncation_reports_total() {
        let (conn, pid) = mem_conn();
        for i in 0..7 {
            add_sym(&conn, pid, &format!("parse_{i}"), "function", "");
        }
        let q = SymbolQuery {
            text: Some("parse".into()),
            limit: 3,
            ..Default::default()
        };
        let out = search_with_total(&conn, pid, &q).unwrap();
        assert_eq!(out.results.len(), 3);
        assert_eq!(out.total, 7);

        // Filter-only path reports a total too.
        let q = SymbolQuery {
            kind: Some(SymbolKind::Function),
            limit: 2,
            ..Default::default()
        };
        let out = search_with_total(&conn, pid, &q).unwrap();
        assert_eq!((out.results.len(), out.total), (2, 7));
    }

    #[test]
    fn test_docs_words_only_match_with_include_docs() {
        let (conn, pid) = mem_conn();
        add_sym(
            &conn,
            pid,
            "handler",
            "function",
            "fn handler() // validates tokens",
        );

        let mut q = SymbolQuery {
            text: Some("validates".into()),
            limit: 10,
            ..Default::default()
        };
        assert!(
            search_with_total(&conn, pid, &q)
                .unwrap()
                .results
                .is_empty()
        );

        q.include_docs = true;
        assert_eq!(
            names(&search_with_total(&conn, pid, &q).unwrap()),
            vec!["handler"]
        );
    }

    #[test]
    fn test_kind_filter_applies_to_text_search() {
        let (conn, pid) = mem_conn();
        add_sym(&conn, pid, "Config", "struct", "");
        add_sym(&conn, pid, "config_path", "function", "");
        let q = SymbolQuery {
            text: Some("config".into()),
            kind: Some(SymbolKind::Struct),
            limit: 10,
            ..Default::default()
        };
        let out = search_with_total(&conn, pid, &q).unwrap();
        assert_eq!(names(&out), vec!["Config"]);
        assert_eq!(out.total, 1);
    }

    #[test]
    fn test_symbol_kind_parse_strict() {
        assert_eq!(SymbolKind::parse("Function"), Some(SymbolKind::Function));
        assert_eq!(SymbolKind::parse("type-alias"), Some(SymbolKind::TypeAlias));
        assert_eq!(SymbolKind::parse("nonsense"), None);
    }
}
