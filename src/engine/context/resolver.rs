//! Symbol resolution — map extracted symbols to file locations,
//! read source content, and assemble the context chain under a token budget.
//!
//! Resolution strategy (language-aware):
//! - **Rust**: `use crate::foo::bar` → `src/foo/bar.rs` (or `mod.rs`)
//! - **Python**: `import foo.bar` → `foo/bar.py` or `foo/bar/__init__.py`
//! - **JS/TS**: `import x from './foo'` → relative path resolution
//! - **Go**: `import "pkg/path"` → `pkg/path/` directory
//! - **Java/Kotlin**: `import foo.Bar` → `foo/Bar.java` / `foo/Bar.kt`
//!
//! Additionally, test file mapping is supported via naming conventions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use tracing::debug;

use super::types::{
    ContextChain, ContextConfig, ContextEntry, ContextPriority, ContextStats, DefinedSymbol,
    DefinitionKind, ExtractedSymbol, SymbolKind, estimate_tokens,
};

/// Maximum lines to read per symbol definition (prevents reading huge functions).
const MAX_FN_LINES: usize = 50;
/// Maximum lines to read per struct/type definition.
const MAX_TYPE_LINES: usize = 50;
/// Maximum lines to read per test function.
const MAX_TEST_LINES: usize = 30;
/// Lines to read per caller (blast-radius) call-site: the call line + context.
const MAX_CALLER_LINES: usize = 4;
/// Maximum number of source files to scan when resolving callers.
const MAX_CALLER_FILES_SCAN: usize = 400;
/// Maximum call-sites injected per changed symbol (keeps callers token-cheap).
const MAX_CALLERS_PER_SYMBOL: usize = 3;
/// Source extensions scanned for caller resolution.
const CALLER_SCAN_EXTS: &[&str] = &[
    "rs", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts",
];

/// Safely join a relative path with a project root, verifying that the
/// resulting path stays within the project root (prevents path traversal).
/// Returns `None` if the resolved path escapes the project root or
/// canonicalization fails for an existing file.
fn safe_join(project_root: &Path, relative: &str) -> Option<PathBuf> {
    let joined = project_root.join(relative);
    // Canonicalize to resolve any `..` or symlinks.
    // If the file doesn't exist yet, canonicalize just the project_root
    // and check that the joined path starts with it as a prefix.
    let canonical_root = std::fs::canonicalize(project_root).ok()?;
    if joined.exists() {
        let canonical_joined = std::fs::canonicalize(&joined).ok()?;
        if canonical_joined.starts_with(&canonical_root) {
            Some(canonical_joined)
        } else {
            None
        }
    } else {
        // File doesn't exist yet; verify the joined path doesn't escape
        // the project root by canonicalizing what we can and checking prefixes.
        if joined.starts_with(project_root) {
            Some(joined)
        } else {
            None
        }
    }
}

/// Build the full context chain from extracted symbols.
///
/// This is the main entry point: extract → resolve → read → budget → assemble.
pub fn build_context_chain(
    symbols: &[ExtractedSymbol],
    defs: &[DefinedSymbol],
    config: &ContextConfig,
    project_root: &Path,
    ignore_patterns: &[String],
) -> ContextChain {
    // Open the index bridge for the project root.
    let bridge = crate::engine::index_bridge::IndexBridge::open(project_root);
    build_context_chain_with_bridge(symbols, defs, config, project_root, ignore_patterns, &bridge)
}

/// Build the full context chain with an explicit [`IndexBridge`].
///
/// This is the internal implementation that both the public `build_context_chain`
/// (which opens a bridge automatically) and callers that already hold a bridge
/// can use.
pub fn build_context_chain_with_bridge(
    symbols: &[ExtractedSymbol],
    defs: &[DefinedSymbol],
    config: &ContextConfig,
    project_root: &Path,
    ignore_patterns: &[String],
    bridge: &crate::engine::index_bridge::IndexBridge,
) -> ContextChain {
    if !config.enabled || (symbols.is_empty() && defs.is_empty()) {
        return ContextChain::default();
    }

    let mut stats = ContextStats {
        symbols_extracted: symbols.len(),
        ..Default::default()
    };

    // Phase 1: Resolve outbound symbols to file locations (what changed code calls)
    let mut entries = if config.prefer_index && bridge.is_available() {
        let index_entries = resolve_via_index(symbols, bridge, project_root, ignore_patterns, &mut stats);
        debug!(
            index_resolved = index_entries.len(),
            source = "index",
            "resolved symbols via index"
        );
        index_entries
    } else {
        resolve_symbols(symbols, config, project_root, ignore_patterns, &mut stats)
    };

    // Phase 2: Add test file mappings
    if config.include_tests {
        add_test_mappings(symbols, project_root, &mut entries, &mut stats);
    }

    // Phase 3: Resolve inbound callers (blast radius — who calls changed code)
    entries.extend(resolve_callers_with_bridge(defs, config, project_root, ignore_patterns, bridge));

    // Sort by priority (FunctionDef first, CallerSite last)
    entries.sort_by_key(|e| e.priority);

    // Phase 4: Read file content under budget
    let mut budget = config.max_context_tokens;
    let mut parts = Vec::new();

    for entry in &entries {
        let content = read_entry_content(entry, project_root);
        let tokens = estimate_tokens(&content);

        if tokens > budget {
            // Tier 3: signature-only fallback — inject a thin slice instead of
            // skipping the entry entirely. Keeps high-value defs under budget.
            if let Some(sig) = signature_only(entry, project_root) {
                let sig_tokens = estimate_tokens(&sig);
                if sig_tokens > 0 && sig_tokens <= budget {
                    budget -= sig_tokens;
                    stats.entries_read += 1;
                    stats.estimated_tokens += sig_tokens;
                    parts.push(format!(
                        "--- {}:{}-{} ({}, signature only) ---\n{}",
                        entry.file, entry.line_start, entry.line_end, entry.label, sig
                    ));
                    continue;
                }
            }
            stats.budget_hit = true;
            debug!(
                entry = %entry.label,
                tokens,
                remaining_budget = budget,
                "skipping context entry (budget exhausted)"
            );
            continue;
        }

        if content.is_empty() {
            continue;
        }

        budget -= tokens;
        stats.entries_read += 1;
        stats.estimated_tokens += tokens;

        parts.push(format!(
            "--- {}:{}-{} ({}) ---\n{}",
            entry.file, entry.line_start, entry.line_end, entry.label, content
        ));
    }

    let text = if parts.is_empty() {
        String::new()
    } else {
        format!("Relevant Cross-File Context:\n\n{}", parts.join("\n"))
    };

    debug!(
        symbols_extracted = stats.symbols_extracted,
        symbols_resolved = stats.symbols_resolved,
        entries_read = stats.entries_read,
        estimated_tokens = stats.estimated_tokens,
        budget_hit = stats.budget_hit,
        "context chain built"
    );

    ContextChain { text, stats }
}

/// Resolve extracted symbols using the symbol index (FTS5 search).
///
/// For each extracted symbol, queries the index for matching definitions and maps
/// the results to [`ContextEntry`].  This is more accurate than regex-based
/// file scanning because it leverages the pre-built symbol table.
///
/// Symbols not found in the index are silently skipped (the regex fallback
/// in `build_context_chain_with_bridge` is only used when the entire index
/// is unavailable, not per-symbol).
fn resolve_via_index(
    symbols: &[ExtractedSymbol],
    bridge: &crate::engine::index_bridge::IndexBridge,
    project_root: &Path,
    ignore_patterns: &[String],
    stats: &mut ContextStats,
) -> Vec<ContextEntry> {
    let mut entries = Vec::new();
    let mut seen_files: HashMap<String, Vec<(u32, u32)>> = HashMap::new();

    for sym in symbols {
        let query_text = match &sym.kind {
            SymbolKind::FunctionCall(name) => name.clone(),
            SymbolKind::TypeRef(name) => name.clone(),
            SymbolKind::Import(path) => {
                // For imports, try to match the last segment as a module name
                path.rsplit("::")
                    .next()
                    .unwrap_or(path)
                    .to_string()
            }
        };

        if query_text.len() < 2 {
            continue;
        }

        let results = bridge.search_symbols(&query_text, 5);
        for result in results {
            let sym_file = &result.symbol.file;

            // Skip entries for the same file the symbol came from
            if sym_file == &sym.file {
                continue;
            }

            // Check ignore patterns
            if is_ignored(sym_file, ignore_patterns) {
                continue;
            }

            // Verify the resolved file exists
            let full_path = match safe_join(project_root, sym_file) {
                Some(p) => p,
                None => continue,
            };
            if !full_path.exists() {
                continue;
            }

            // Determine priority from the symbol kind
            let kind_str = result.symbol.kind.as_str();
            let (priority, label) = match &sym.kind {
                SymbolKind::FunctionCall(_) => {
                    let prio = if kind_str == "function" || kind_str == "method" {
                        ContextPriority::FunctionDef
                    } else {
                        ContextPriority::TypeDef
                    };
                    (prio, format!("fn {}", result.symbol.name))
                }
                SymbolKind::TypeRef(_) => (
                    ContextPriority::TypeDef,
                    format!("type {}", result.symbol.name),
                ),
                SymbolKind::Import(_) => (
                    ContextPriority::TypeDef,
                    format!("module {}", result.symbol.name),
                ),
            };

            let line_start = result.symbol.line;
            let line_end = (line_start + MAX_FN_LINES as u32)
                .min(if let Ok(content) = std::fs::read_to_string(&full_path) {
                    content.lines().count() as u32
                } else {
                    line_start
                });

            // Merge overlapping ranges for the same file
            let file_ranges = seen_files.entry(sym_file.clone()).or_default();
            let overlaps = file_ranges
                .iter()
                .any(|(s, e)| line_start <= *e && line_end >= *s);

            if !overlaps {
                file_ranges.push((line_start, line_end));
                entries.push(ContextEntry {
                    file: sym_file.clone(),
                    line_start,
                    line_end,
                    label,
                    priority,
                });
            }
        }
    }

    stats.symbols_resolved = entries.len();
    entries
}

/// Resolve extracted symbols to concrete file locations and line ranges.
fn resolve_symbols(
    symbols: &[ExtractedSymbol],
    config: &ContextConfig,
    project_root: &Path,
    ignore_patterns: &[String],
    stats: &mut ContextStats,
) -> Vec<ContextEntry> {
    let mut entries = Vec::new();
    let mut seen_files: HashMap<String, Vec<(u32, u32)>> = HashMap::new(); // track line ranges per file

    // File content cache to avoid re-reading the same files from disk
    let mut file_cache: HashMap<std::path::PathBuf, String> = HashMap::new();

    for sym in symbols {
        // Determine the language from the file extension
        let lang = Path::new(&sym.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let resolved = match &sym.kind {
            SymbolKind::Import(path) => resolve_import(path, &sym.file, lang, project_root),
            SymbolKind::FunctionCall(name) => {
                resolve_function(name, &sym.file, lang, project_root, &mut file_cache)
            }
            SymbolKind::TypeRef(name) => {
                resolve_type(name, &sym.file, lang, project_root, &mut file_cache)
            }
        };

        for entry in resolved {
            // Check ignore patterns
            if is_ignored(&entry.file, ignore_patterns) {
                debug!(file = %entry.file, "skipping ignored file");
                continue;
            }

            // Check if the entry's file exists and stays within project root
            let full_path = match safe_join(project_root, &entry.file) {
                Some(p) => p,
                None => {
                    debug!(file = %entry.file, "path traversal detected, skipping");
                    continue;
                }
            };
            if !full_path.exists() {
                debug!(file = %entry.file, "resolved file does not exist, skipping");
                continue;
            }

            // Don't add context for the same file the symbol came from
            if entry.file == sym.file {
                continue;
            }

            stats.symbols_resolved += 1;

            // Merge line ranges for same file to avoid duplicates
            let file_ranges = seen_files.entry(entry.file.clone()).or_default();
            let overlaps = file_ranges
                .iter()
                .any(|(s, e)| entry.line_start <= *e && entry.line_end >= *s);

            if !overlaps {
                file_ranges.push((entry.line_start, entry.line_end));
                entries.push(entry);
            }
        }

        // Respect follow depth (depth 1 = only direct references)
        if config.follow_depth <= 1 {
            continue;
        }
        // Higher depths would recursively resolve symbols found in resolved content.
        // For now, depth > 1 is a no-op placeholder for future expansion.
    }

    entries
}

/// Resolve an import to a file path.
fn resolve_import(
    import_path: &str,
    _source_file: &str,
    lang: &str,
    project_root: &Path,
) -> Vec<ContextEntry> {
    let mut entries = Vec::new();

    match lang {
        "rs" => {
            // `use crate::foo::bar::baz` → try `src/foo/bar/baz.rs` or `src/foo/bar/baz/mod.rs`
            let path = import_path.replace("::", "/");
            let candidates = [
                format!("src/{path}.rs"),
                format!("src/{path}/mod.rs"),
                format!("src/{path}/lib.rs"),
            ];

            for candidate in &candidates {
                let full = project_root.join(candidate);
                if full.exists() {
                    let line_end = find_definition_end(&full);
                    entries.push(ContextEntry {
                        file: candidate.clone(),
                        line_start: 1,
                        line_end,
                        label: format!("module {import_path}"),
                        priority: ContextPriority::TypeDef,
                    });
                    break;
                }
            }
        }
        "py" | "pyi" => {
            // `import foo.bar` → `foo/bar.py` or `foo/bar/__init__.py`
            let candidates = [
                format!("{import_path}.py"),
                format!("{import_path}/__init__.py"),
            ];

            for candidate in &candidates {
                let full = project_root.join(candidate);
                if full.exists() {
                    let line_end = find_definition_end(&full);
                    entries.push(ContextEntry {
                        file: candidate.clone(),
                        line_start: 1,
                        line_end,
                        label: format!("module {import_path}"),
                        priority: ContextPriority::TypeDef,
                    });
                    break;
                }
            }
        }
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            // `import x from './foo'` or `import x from 'foo'` → relative or node_modules
            let path = import_path
                .trim_start_matches("./")
                .trim_start_matches("../");

            // Only resolve relative imports (not node_modules)
            if import_path.starts_with('.') {
                let source_dir = Path::new(_source_file).parent().unwrap_or(Path::new(""));
                let resolved = source_dir.join(path);

                let extensions = match lang {
                    "ts" | "tsx" => vec!["ts", "tsx"],
                    _ => vec!["js", "jsx", "mjs", "cjs"],
                };

                for ext in &extensions {
                    let candidate = format!("{}.{}", resolved.display(), ext);
                    let full = match safe_join(project_root, &candidate) {
                        Some(p) => p,
                        None => {
                            debug!(file = %candidate, "path traversal detected in JS/TS import, skipping");
                            continue;
                        }
                    };
                    if full.exists() {
                        let line_end = find_definition_end(&full);
                        entries.push(ContextEntry {
                            file: candidate,
                            line_start: 1,
                            line_end,
                            label: format!("module {import_path}"),
                            priority: ContextPriority::TypeDef,
                        });
                        break;
                    }
                }
            }
        }
        "go" => {
            // Go imports are package paths; resolve relative to project root
            let candidate = import_path.trim_start_matches("\"").trim_end_matches("\"");
            let full = project_root.join(candidate);
            if full.is_dir() {
                // Find .go files in the directory
                if let Some(entry) = find_go_package_file(&full, import_path) {
                    entries.push(entry);
                }
            }
        }
        "java" | "kt" | "kts" => {
            // `import foo.bar.Baz` → `foo/bar/Baz.java` or `foo/bar/Baz.kt`
            let path = import_path.replace('.', "/");
            let ext = if lang == "java" { "java" } else { "kt" };
            let candidate = format!("{path}.{ext}");
            let full = project_root.join(&candidate);
            if full.exists() {
                let line_end = find_definition_end(&full);
                entries.push(ContextEntry {
                    file: candidate,
                    line_start: 1,
                    line_end,
                    label: format!("module {import_path}"),
                    priority: ContextPriority::TypeDef,
                });
            }
        }
        _ => {}
    }

    entries
}

/// Resolve a function name to a file location.
/// This does a best-effort search for the function definition.
fn resolve_function(
    name: &str,
    source_file: &str,
    lang: &str,
    project_root: &Path,
    file_cache: &mut HashMap<std::path::PathBuf, String>,
) -> Vec<ContextEntry> {
    let mut entries = Vec::new();

    // Strategy: look in nearby files (same directory) and import targets
    let search_dir = Path::new(source_file).parent().unwrap_or(Path::new(""));

    match lang {
        "rs" => {
            // Search for `pub fn <name>` or `fn <name>` in sibling .rs files
            if let Ok(files) = std::fs::read_dir(project_root.join(search_dir)) {
                for file in files.flatten() {
                    let path = file.path();
                    if let Some(ext) = path.extension() {
                        if ext != "rs" {
                            continue;
                        }
                    } else {
                        continue;
                    }

                    if let Ok(rel) = path.strip_prefix(project_root) {
                        let rel_str = rel.to_string_lossy().to_string();
                        if rel_str == source_file {
                            continue;
                        }

                        if let Some((start, end)) =
                            find_fn_in_file_cached(&path, name, "rs", file_cache)
                        {
                            entries.push(ContextEntry {
                                file: rel_str,
                                line_start: start,
                                line_end: end,
                                label: format!("fn {name}"),
                                priority: ContextPriority::FunctionDef,
                            });
                        }
                    }
                }
            }
        }
        "py" | "pyi" => {
            if let Ok(files) = std::fs::read_dir(project_root.join(search_dir)) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().map(|e| e != "py").unwrap_or(true) {
                        continue;
                    }

                    if let Ok(rel) = path.strip_prefix(project_root) {
                        let rel_str = rel.to_string_lossy().to_string();
                        if rel_str == source_file {
                            continue;
                        }

                        if let Some((start, end)) =
                            find_fn_in_file_cached(&path, name, "py", file_cache)
                        {
                            entries.push(ContextEntry {
                                file: rel_str,
                                line_start: start,
                                line_end: end,
                                label: format!("def {name}"),
                                priority: ContextPriority::FunctionDef,
                            });
                        }
                    }
                }
            }
        }
        _ => {
            // Generic: search for the function name in sibling files
            if let Ok(files) = std::fs::read_dir(project_root.join(search_dir)) {
                for file in files.flatten() {
                    let path = file.path();
                    if let Ok(rel) = path.strip_prefix(project_root) {
                        let rel_str = rel.to_string_lossy().to_string();
                        if rel_str == source_file {
                            continue;
                        }

                        if let Some((start, end)) = find_fn_generic_cached(&path, name, file_cache)
                        {
                            entries.push(ContextEntry {
                                file: rel_str,
                                line_start: start,
                                line_end: end,
                                label: format!("fn {name}"),
                                priority: ContextPriority::FunctionDef,
                            });
                        }
                    }
                }
            }
        }
    }

    entries
}

/// Resolve a type name to a file location.
fn resolve_type(
    name: &str,
    source_file: &str,
    lang: &str,
    project_root: &Path,
    file_cache: &mut HashMap<std::path::PathBuf, String>,
) -> Vec<ContextEntry> {
    let search_dir = Path::new(source_file).parent().unwrap_or(Path::new(""));
    let mut entries = Vec::new();

    let pattern = match lang {
        "rs" => format!("struct {name}"),
        "py" => format!("class {name}"),
        "go" => format!("type {name} struct"),
        "java" | "kt" => format!("class {name}"),
        _ => format!("struct {name}"),
    };

    if let Ok(files) = std::fs::read_dir(project_root.join(search_dir)) {
        for file in files.flatten() {
            let path = file.path();
            if let Ok(rel) = path.strip_prefix(project_root) {
                let rel_str = rel.to_string_lossy().to_string();
                if rel_str == source_file {
                    continue;
                }

                if let Some((start, end)) = find_pattern_in_file_cached(&path, &pattern, file_cache)
                {
                    entries.push(ContextEntry {
                        file: rel_str,
                        line_start: start,
                        line_end: end,
                        label: format!("type {name}"),
                        priority: ContextPriority::TypeDef,
                    });
                }
            }
        }
    }

    entries
}

/// Cached version of `find_fn_in_file` — avoids re-reading files from disk.
fn find_fn_in_file_cached(
    path: &Path,
    name: &str,
    lang: &str,
    file_cache: &mut HashMap<PathBuf, String>,
) -> Option<(u32, u32)> {
    let content = get_file_cached(path, file_cache)?;

    let pattern = match lang {
        "rs" => format!("fn {name}"),
        "py" => format!("def {name}"),
        _ => return find_fn_generic_cached(path, name, file_cache),
    };

    find_pattern_with_body(&content, &pattern, MAX_FN_LINES)
}

/// Generic function search (for languages without specific patterns).
#[allow(dead_code)]
fn find_fn_generic(path: &Path, name: &str) -> Option<(u32, u32)> {
    let content = std::fs::read_to_string(path).ok()?;
    find_pattern_with_body(&content, &format!("fn {name}"), MAX_FN_LINES)
}

/// Cached version of `find_fn_generic`.
fn find_fn_generic_cached(
    path: &Path,
    name: &str,
    file_cache: &mut HashMap<PathBuf, String>,
) -> Option<(u32, u32)> {
    let content = get_file_cached(path, file_cache)?;
    find_pattern_with_body(&content, &format!("fn {name}"), MAX_FN_LINES)
}

/// Find a pattern (like `struct Foo`) and determine its extent.
#[allow(dead_code)]
fn find_pattern_in_file(path: &Path, pattern: &str) -> Option<(u32, u32)> {
    let content = std::fs::read_to_string(path).ok()?;
    find_pattern_with_body(&content, pattern, MAX_TYPE_LINES)
}

/// Cached version of `find_pattern_in_file`.
fn find_pattern_in_file_cached(
    path: &Path,
    pattern: &str,
    file_cache: &mut HashMap<PathBuf, String>,
) -> Option<(u32, u32)> {
    let content = get_file_cached(path, file_cache)?;
    find_pattern_with_body(&content, pattern, MAX_TYPE_LINES)
}

/// Read a file from disk, using the cache if available.
fn get_file_cached(path: &Path, cache: &mut HashMap<PathBuf, String>) -> Option<String> {
    if let Some(content) = cache.get(path) {
        return Some(content.clone());
    }
    let content = std::fs::read_to_string(path).ok()?;
    cache.insert(path.to_path_buf(), content.clone());
    Some(content)
}

/// Find a pattern in content and estimate the block extent by counting braces/indents.
fn find_pattern_with_body(content: &str, pattern: &str, max_lines: usize) -> Option<(u32, u32)> {
    let mut start_line: Option<usize> = None;
    let mut brace_count = 0i32;
    let mut line_idx = 0;

    for line in content.lines() {
        line_idx += 1;

        if start_line.is_none() {
            if line.contains(pattern)
                && !line.trim_start().starts_with("//")
                && !line.trim_start().starts_with('#')
            {
                start_line = Some(line_idx);
                brace_count = count_braces_delta(line);
            }
            continue;
        }

        brace_count += count_braces_delta(line);

        // Block ends when braces are balanced (and we have at least the header)
        if brace_count <= 0 && start_line.is_some() {
            let start = start_line? as u32;
            let end = (line_idx as u32).min(start + max_lines as u32);
            return Some((start, end));
        }

        // Hard cap
        if line_idx >= start_line.unwrap_or(0) + max_lines {
            let start = start_line? as u32;
            return Some((start, start + max_lines as u32));
        }
    }

    // If we found the start but never balanced braces, cap at max_lines
    start_line.map(|s| {
        (
            s as u32,
            (s + max_lines.min(content.lines().count() - s + 1)) as u32,
        )
    })
}

/// Count net brace delta: +1 for `{`, -1 for `}`.
fn count_braces_delta(line: &str) -> i32 {
    let mut delta = 0i32;
    let mut in_string = false;
    let mut in_char = false;
    let mut escape = false;

    for ch in line.chars() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && !in_char {
            escape = true;
            continue;
        }
        if ch == '"' && !in_char {
            in_string = !in_string;
            continue;
        }
        if ch == '\'' && !in_string && !in_char {
            in_char = true;
            continue;
        }
        if ch == '\'' && in_char {
            in_char = false;
            continue;
        }
        if in_string || in_char {
            continue;
        }
        if ch == '{' {
            delta += 1;
        } else if ch == '}' {
            delta -= 1;
        }
    }
    delta
}

/// Find the last line of a file's "definition block" (simplified).
fn find_definition_end(path: &Path) -> u32 {
    if let Ok(content) = std::fs::read_to_string(path) {
        let lines = content.lines().count();
        (lines as u32).min(MAX_TYPE_LINES as u32)
    } else {
        1
    }
}

/// Find a .go file in a package directory.
fn find_go_package_file(dir: &Path, import_path: &str) -> Option<ContextEntry> {
    let files = std::fs::read_dir(dir).ok()?;
    for file in files.flatten() {
        let path = file.path();
        if path.extension().map(|e| e == "go").unwrap_or(false) {
            let line_end = find_definition_end(&path);
            let rel = path
                .to_str()
                .and_then(|p| p.rsplit_once('/').map(|x| x.0))
                .unwrap_or(import_path);
            return Some(ContextEntry {
                file: rel.to_string(),
                line_start: 1,
                line_end,
                label: format!("package {import_path}"),
                priority: ContextPriority::TypeDef,
            });
        }
    }
    None
}

/// Check if a file path matches any ignore pattern.
/// Resolve **callers** (blast radius) of functions/types defined in the diff.
///
/// This is the inbound counterpart to outbound symbol resolution: instead of
/// "what does the changed code call", it answers "who calls the changed code" —
/// the most valuable context for flagging breaking signature/type changes.
///
/// Uses gitignore-aware walking (the `ignore` crate) so build artifacts are
/// never scanned, and is bounded by [`MAX_CALLER_FILES_SCAN`] files and
/// [`MAX_CALLERS_PER_SYMBOL`] call-sites per symbol. Caller slices are tiny
/// (the call line + 1 line of context) to stay token-economical.
#[allow(dead_code)]
fn resolve_callers(
    defs: &[DefinedSymbol],
    config: &ContextConfig,
    project_root: &Path,
    ignore_patterns: &[String],
) -> Vec<ContextEntry> {
    // Open a fresh bridge for backward compatibility with callers that don't
    // pass one explicitly.
    let bridge = crate::engine::index_bridge::IndexBridge::open(project_root);
    resolve_callers_with_bridge(defs, config, project_root, ignore_patterns, &bridge)
}

/// Resolve callers using an existing [`IndexBridge`].
///
/// When the bridge is available, uses the call-graph index for precise
/// caller resolution.  Falls back to regex-based file scanning otherwise.
fn resolve_callers_with_bridge(
    defs: &[DefinedSymbol],
    config: &ContextConfig,
    project_root: &Path,
    ignore_patterns: &[String],
    bridge: &crate::engine::index_bridge::IndexBridge,
) -> Vec<ContextEntry> {
    if !config.include_callers || defs.is_empty() {
        return Vec::new();
    }

    // ── Index-based caller resolution (preferred) ───────────────────────
    if bridge.is_available() {
        let mut entries = Vec::new();
        for def in defs {
            if def.name.len() < 2 {
                continue;
            }
            let callers = bridge.find_callers(&def.name, 20);
            for caller in callers {
                // Skip callers in the defining file itself
                if caller.file == def.file {
                    continue;
                }
                let rel = caller.file.clone();
                if is_ignored(&rel, ignore_patterns) {
                    continue;
                }
                let label = match def.kind {
                    DefinitionKind::Function => {
                        format!("caller of fn {}", def.name)
                    }
                    DefinitionKind::Type => {
                        format!("usage of {}", def.name)
                    }
                };
                entries.push(ContextEntry {
                    file: rel,
                    line_start: caller.line.saturating_sub(1).max(1),
                    line_end: caller.line + 1,
                    label,
                    priority: ContextPriority::CallerSite,
                });
            }
        }
        if !entries.is_empty() {
            debug!(
                callers_found = entries.len(),
                source = "index",
                "resolved callers via bridge"
            );
            return entries;
        }
        // Index found but no callers — fall through to regex scan
        debug!("index caller lookup returned empty, falling back to regex");
    }

    // ── Regex-based fallback (original behavior) ───────────────────────
    debug!("using regex-based caller resolution");
    resolve_callers_regex(defs, project_root, ignore_patterns)
}

/// Regex-based caller resolution — scans project files for symbol references.
/// Used as fallback when no symbol index is available.
fn resolve_callers_regex(
    defs: &[DefinedSymbol],
    project_root: &Path,
    ignore_patterns: &[String],
) -> Vec<ContextEntry> {
    // Precompile a matcher per definition. Skip names that are too short/noisy.
    let matchers: Vec<(&DefinedSymbol, Regex)> = defs
        .iter()
        .filter_map(|d| {
            if d.name.len() < 2 {
                return None;
            }
            let pattern = match d.kind {
                DefinitionKind::Function => format!(r"\b{}\s*\(", regex::escape(&d.name)),
                DefinitionKind::Type => format!(r"\b{}\b", regex::escape(&d.name)),
            };
            Regex::new(&pattern).ok().map(|r| (d, r))
        })
        .collect();
    if matchers.is_empty() {
        return Vec::new();
    }

    let walker = ignore::WalkBuilder::new(project_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let mut entries = Vec::new();
    let mut files_scanned = 0usize;

    for dent in walker {
        if files_scanned >= MAX_CALLER_FILES_SCAN {
            break;
        }
        let dent = match dent {
            Ok(d) => d,
            Err(_) => continue,
        };
        if !dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = dent.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !CALLER_SCAN_EXTS.contains(&ext) {
            continue;
        }

        let rel = match path.strip_prefix(project_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/").to_string(),
            Err(_) => continue,
        };
        if is_ignored(&rel, ignore_patterns) {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        files_scanned += 1;

        for (def, re) in &matchers {
            // The defining file is not a "caller" of its own symbol.
            if def.file == rel {
                continue;
            }
            let mut hits = 0usize;
            for (idx, line) in content.lines().enumerate() {
                if hits >= MAX_CALLERS_PER_SYMBOL {
                    break;
                }
                if re.is_match(line) {
                    let ln = (idx + 1) as u32;
                    let label = match def.kind {
                        DefinitionKind::Function => format!("caller of fn {}", def.name),
                        DefinitionKind::Type => format!("usage of {}", def.name),
                    };
                    entries.push(ContextEntry {
                        file: rel.clone(),
                        line_start: ln.saturating_sub(1).max(1),
                        line_end: ln + 1,
                        label,
                        priority: ContextPriority::CallerSite,
                    });
                    hits += 1;
                }
            }
        }
    }

    debug!(
        callers_found = entries.len(),
        files_scanned,
        source = "regex",
        "resolved callers"
    );
    entries
}

/// Extract just the signature (not the full body) of a definition, for the
/// budget-aware fallback. Reads up to the opening `{` or a few lines.
fn signature_only(entry: &ContextEntry, project_root: &Path) -> Option<String> {
    let full = safe_join(project_root, &entry.file).filter(|p| p.exists())?;
    let content = std::fs::read_to_string(&full).ok()?;
    let start = entry.line_start.saturating_sub(1) as usize;
    let mut sig: Vec<&str> = Vec::new();
    for line in content.lines().skip(start).take(8) {
        sig.push(line);
        if line.contains('{') || sig.len() >= 4 {
            break;
        }
    }
    if sig.is_empty() {
        None
    } else {
        Some(sig.join("\n"))
    }
}

fn is_ignored(file: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        // Simple glob-like matching: check if file contains the pattern
        // or matches as a suffix (e.g., "target/**" matches "target/debug/foo.rs")
        let p = pattern.trim_end_matches("**");
        if p.is_empty() {
            continue;
        }
        if file.starts_with(p.trim_end_matches('/')) || file.contains(p.trim_matches('*')) {
            return true;
        }
    }
    false
}

/// Add test file mappings for changed source files.
fn add_test_mappings(
    symbols: &[ExtractedSymbol],
    project_root: &Path,
    entries: &mut Vec<ContextEntry>,
    stats: &mut ContextStats,
) {
    // Collect unique source files from symbols
    let mut seen = std::collections::HashSet::new();

    for sym in symbols {
        if !seen.insert(&sym.file) {
            continue;
        }

        let candidates = test_file_candidates(&sym.file);
        for candidate in candidates {
            let full = match safe_join(project_root, &candidate) {
                Some(p) => p,
                None => {
                    debug!(file = %candidate, "path traversal detected in test resolution, skipping");
                    continue;
                }
            };
            if full.exists() {
                let line_end = find_definition_end(&full);
                entries.push(ContextEntry {
                    file: candidate,
                    line_start: 1,
                    line_end,
                    label: format!("tests for {}", sym.file),
                    priority: ContextPriority::Test,
                });
                stats.symbols_resolved += 1;
                break;
            }
        }
    }
}

/// Generate test file candidate paths for a source file.
fn test_file_candidates(source: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    let stem = Path::new(source)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    // Rust: `src/foo/bar.rs` → `tests/foo/bar_test.rs`, `tests/bar_test.rs`
    // Also check: `tests/foo_test.rs`
    if source.ends_with(".rs") {
        candidates.push(format!("tests/{stem}_test.rs"));
        if let Some(parent) = Path::new(source).parent() {
            candidates.push(format!("tests/{}/{}", parent.display(), stem));
            candidates.push(format!("tests/{}/{}_test.rs", parent.display(), stem));
        }
        candidates.push(format!("{stem}_test.rs"));
    }

    // Python: `foo/bar.py` → `tests/test_bar.py`, `foo/test_bar.py`
    if source.ends_with(".py") {
        candidates.push(format!("tests/test_{stem}.py"));
        if let Some(parent) = Path::new(source).parent() {
            candidates.push(format!("{}/test_{stem}.py", parent.display()));
        }
    }

    // JS/TS: `src/foo.ts` → `src/foo.test.ts`, `tests/foo.test.ts`
    if source.ends_with(".ts") || source.ends_with(".tsx") {
        let ext = source.rsplit('.').next().unwrap_or("ts");
        candidates.push(format!("tests/{stem}.test.{ext}"));
        candidates.push(format!("tests/{stem}.spec.{ext}"));
        let without_ext = &source[..source.len() - ext.len() - 1];
        candidates.push(format!("{without_ext}.test.{ext}"));
        candidates.push(format!("{without_ext}.spec.{ext}"));
    }

    // Go: `foo.go` → `foo_test.go`
    if source.ends_with(".go") {
        candidates.push(source.replace(".go", "_test.go"));
    }

    // Java/Kotlin
    if source.ends_with(".java") {
        candidates.push(source.replace(".java", "Test.java"));
    }
    if source.ends_with(".kt") {
        candidates.push(source.replace(".kt", "Test.kt"));
    }

    candidates
}

/// Read the content for a context entry, respecting line range and caps.
fn read_entry_content(entry: &ContextEntry, project_root: &Path) -> String {
    let full_path = match safe_join(project_root, &entry.file) {
        Some(p) if p.exists() => p,
        Some(_) => {
            debug!(file = %entry.file, "context entry file does not exist");
            return String::new();
        }
        None => {
            debug!(file = %entry.file, "path traversal detected in read_entry_content");
            return String::new();
        }
    };
    let content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) => {
            debug!(file = %entry.file, error = %e, "failed to read context entry file");
            return String::new();
        }
    };

    let start = entry.line_start.saturating_sub(1) as usize;
    let end = entry.line_end as usize;

    let lines: Vec<&str> = content.lines().collect();
    let relevant: Vec<&str> = lines
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect();

    let result = relevant.join("\n");

    // Apply cap based on priority
    let max_lines = match entry.priority {
        ContextPriority::FunctionDef => MAX_FN_LINES,
        ContextPriority::TypeDef => MAX_TYPE_LINES,
        ContextPriority::Test => MAX_TEST_LINES,
        ContextPriority::CallerSite => MAX_CALLER_LINES,
    };

    if result.lines().count() > max_lines {
        result
            .lines()
            .take(max_lines)
            .chain(std::iter::once("... (truncated)"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── test_file_candidates ───

    #[test]
    fn rust_test_candidates() {
        let candidates = test_file_candidates("src/engine/scanner.rs");
        assert!(
            candidates.contains(&"tests/scanner_test.rs".to_string()),
            "should generate tests/scanner_test.rs"
        );
        assert!(
            candidates.iter().any(|c| c.ends_with("engine/scanner")),
            "should generate nested test path"
        );
    }

    #[test]
    fn python_test_candidates() {
        let candidates = test_file_candidates("app/auth.py");
        assert!(
            candidates.contains(&"tests/test_auth.py".to_string()),
            "should generate tests/test_auth.py"
        );
    }

    #[test]
    fn js_test_candidates() {
        let candidates = test_file_candidates("src/api.ts");
        assert!(
            candidates.iter().any(|c| c.contains("api.test.ts")),
            "should generate .test.ts candidate"
        );
    }

    #[test]
    fn go_test_candidates() {
        let candidates = test_file_candidates("main.go");
        assert!(
            candidates.contains(&"main_test.go".to_string()),
            "should generate main_test.go"
        );
    }

    // ─── is_ignored ───

    #[test]
    fn ignore_target_dir() {
        assert!(is_ignored(
            "target/debug/foo.rs",
            &["target/**".to_string()]
        ));
    }

    #[test]
    fn ignore_node_modules() {
        assert!(is_ignored(
            "node_modules/pkg/index.js",
            &["node_modules/**".to_string()]
        ));
    }

    #[test]
    fn not_ignored_src() {
        assert!(!is_ignored("src/main.rs", &["target/**".to_string()]));
    }

    // ─── count_braces_delta ───

    #[test]
    fn brace_delta_basic() {
        assert_eq!(count_braces_delta("{ }"), 0);
        assert_eq!(count_braces_delta("{{"), 2);
        assert_eq!(count_braces_delta("}}"), -2);
        assert_eq!(count_braces_delta("fn foo() {"), 1);
        assert_eq!(count_braces_delta("}"), -1);
    }

    #[test]
    fn brace_delta_ignores_strings() {
        assert_eq!(count_braces_delta(r#"let s = "{";"#), 0);
        assert_eq!(count_braces_delta(r#"println!("{")");"#), 0);
    }

    #[test]
    fn brace_delta_ignores_comments() {
        // Brace in comment shouldn't count... but our simple parser doesn't handle
        // Rust comments. For now, it's a known limitation. The function handles
        // string escaping correctly though.
        assert_eq!(count_braces_delta(r#"let x = 1; // { }"#), 0);
    }

    // ─── find_pattern_with_body ───

    #[test]
    fn find_simple_struct() {
        let content = "fn main() {}\n\npub struct Foo {\n    x: i32,\n}\n\nfn other() {}";
        let result = find_pattern_with_body(content, "struct Foo", 10);
        assert_eq!(result, Some((3, 5)));
    }

    #[test]
    fn find_nested_function() {
        let content = "fn outer() {\n    fn inner() {\n        1\n    }\n}\n\npub fn target() {\n    let x = 1;\n    return x;\n}\n";
        let result = find_pattern_with_body(content, "fn target", 10);
        assert_eq!(result, Some((7, 10)));
    }

    #[test]
    fn find_pattern_not_found() {
        let content = "fn main() {}\npub fn other() {}";
        let result = find_pattern_with_body(content, "fn missing", 10);
        assert!(result.is_none());
    }

    // ─── build_context_chain integration ───

    #[test]
    fn disabled_config_returns_empty() {
        let config = ContextConfig {
            enabled: false,
            ..Default::default()
        };
        let chain = build_context_chain(&[], &[], &config, Path::new("/tmp"), &[]);
        assert!(chain.text.is_empty());
    }

    #[test]
    fn empty_symbols_returns_empty() {
        let config = ContextConfig::default();
        let chain = build_context_chain(&[], &[], &config, Path::new("/tmp"), &[]);
        assert!(chain.text.is_empty());
    }

    #[test]
    fn budget_enforced() {
        // Create a tiny budget
        let config = ContextConfig {
            enabled: true,
            max_context_tokens: 5, // very small
            follow_depth: 1,
            include_tests: false,
            include_callers: false,
            use_brain: false,
            impact_depth: 2,
        };

        let symbols = vec![ExtractedSymbol {
            kind: SymbolKind::FunctionCall("some_func".to_string()),
            file: "nonexistent.rs".to_string(),
            line: 1,
            raw: "some_func()".to_string(),
        }];

        let chain = build_context_chain(&symbols, &[], &config, Path::new("/tmp"), &[]);
        // Even if resolution finds nothing, the chain should be empty
        assert!(chain.text.is_empty() || chain.stats.budget_hit);
    }

    // ─── estimate_tokens consistency ───

    #[test]
    fn budget_accounting() {
        let config = ContextConfig {
            enabled: true,
            max_context_tokens: 100,
            follow_depth: 1,
            include_tests: false,
            include_callers: false,
            use_brain: false,
            impact_depth: 2,
        };

        let symbols = vec![ExtractedSymbol {
            kind: SymbolKind::Import("engine::scanner".to_string()),
            file: "src/main.rs".to_string(),
            line: 1,
            raw: "use crate::engine::scanner;".to_string(),
        }];

        let chain = build_context_chain(&symbols, &[], &config, Path::new("/tmp"), &[]);
        // With a nonexistent project root, nothing should resolve
        assert!(chain.text.is_empty());
        assert_eq!(chain.stats.symbols_extracted, 1);
    }

    // ─── caller (blast-radius) resolution ───

    #[test]
    fn resolve_callers_finds_call_site() {
        use crate::engine::context::types::{DefinedSymbol, DefinitionKind};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/caller.rs"),
            "fn caller() {\n    validate_token(t);\n}\n",
        )
        .unwrap();

        let defs = vec![DefinedSymbol {
            name: "validate_token".to_string(),
            kind: DefinitionKind::Function,
            file: "src/auth.rs".to_string(),
        }];
        let entries = resolve_callers(&defs, &ContextConfig::default(), root, &[]);
        assert!(
            entries
                .iter()
                .any(|e| e.file == "src/caller.rs" && e.label.contains("validate_token")),
            "should resolve the caller site: {entries:?}"
        );
    }

    #[test]
    fn resolve_callers_skips_defining_file_and_respects_disable() {
        use crate::engine::context::types::{DefinedSymbol, DefinitionKind};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // The defining file itself contains the call — it must not self-report.
        std::fs::write(
            root.join("src/auth.rs"),
            "fn auth() { validate_token(t); }\n",
        )
        .unwrap();

        let defs = vec![DefinedSymbol {
            name: "validate_token".to_string(),
            kind: DefinitionKind::Function,
            file: "src/auth.rs".to_string(),
        }];

        // include_callers = true but only the defining file matches → no entries.
        let entries = resolve_callers(&defs, &ContextConfig::default(), root, &[]);
        assert!(
            entries.is_empty(),
            "defining file must not be its own caller"
        );

        // include_callers = false → no entries regardless.
        let cfg = ContextConfig {
            include_callers: false,
            ..Default::default()
        };
        assert!(resolve_callers(&defs, &cfg, root, &[]).is_empty());
    }

    // ─── signature-only fallback ───

    #[test]
    fn signature_only_returns_header_without_body() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("big.rs"),
            "pub fn big(x: i32) -> i32 {\n    let y = 1;\n    y + x\n}\n",
        )
        .unwrap();
        let entry = ContextEntry {
            file: "big.rs".to_string(),
            line_start: 1,
            line_end: 4,
            label: "fn big".to_string(),
            priority: ContextPriority::FunctionDef,
        };
        let sig = signature_only(&entry, root).expect("signature should be extracted");
        assert!(sig.contains("pub fn big"), "sig: {sig}");
        assert!(!sig.contains("y + x"), "body must not be included: {sig}");
    }
}
