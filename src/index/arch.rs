//! Architecture overview: symbols grouped by crate (Cargo package, which
//! covers every workspace member) and then by module path, plus call-edge
//! counts between those groups.
//!
//! Crates are discovered on disk: the nearest ancestor directory of an indexed
//! file that holds a `Cargo.toml` with a `[package]` table owns the file.
//! Files outside any package (non-Rust projects, scripts) fall under
//! [`NO_CRATE`] and are grouped by directory.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;

/// Group name for files that do not belong to any Cargo package.
pub const NO_CRATE: &str = "(no crate)";

/// Module name for files at a crate's root (`main.rs`, `lib.rs`, ...).
const ROOT_MODULE: &str = "(root)";

/// A Cargo package found on disk: its directory (relative to the project
/// root, `/`-separated, empty for the root) and its `[package].name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateRoot {
    pub dir: String,
    pub name: String,
}

/// Architecture overview result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchOverview {
    /// Legacy view: symbols per top-level directory.
    pub modules: Vec<ModuleInfo>,
    pub edge_counts: Vec<(String, i64)>,
    /// Crates (largest first), each with its modules.
    pub crates: Vec<CrateGroup>,
    /// Resolved call edges between different crates, most frequent first.
    pub crate_edges: Vec<GroupEdge>,
    /// Resolved call edges between different modules (`crate::module`).
    pub module_edges: Vec<GroupEdge>,
    /// Call rows whose callee could not be tied to a single group (std or
    /// external calls, or names defined in several places).
    pub unresolved_calls: usize,
}

/// One crate with its modules.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrateGroup {
    pub name: String,
    /// Crate directory relative to the project root (`.` for the root).
    pub dir: String,
    pub symbol_count: usize,
    pub modules: Vec<ModuleInfo>,
}

/// Module-level statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModuleInfo {
    pub name: String,
    pub symbol_count: usize,
    /// Number of distinct symbol kinds in the module.
    pub edge_types: usize,
}

/// Call edge between two groups.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GroupEdge {
    pub from: String,
    pub to: String,
    pub count: usize,
}

/// Read `[package].name` from a Cargo manifest, if it has one.
fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// Find the Cargo packages that own any of `files` (project-relative paths).
pub fn discover_crates(root: &Path, files: &[String]) -> Vec<CrateRoot> {
    let mut dirs: HashSet<String> = HashSet::new();
    for f in files {
        let mut cur = Path::new(f).parent();
        while let Some(d) = cur {
            dirs.insert(d.to_string_lossy().replace('\\', "/"));
            cur = d.parent();
        }
    }
    let mut crates: Vec<CrateRoot> = dirs
        .into_iter()
        .filter_map(|dir| {
            let manifest = root.join(&dir).join("Cargo.toml");
            package_name(&manifest).map(|name| CrateRoot { dir, name })
        })
        .collect();
    crates.sort_by(|a, b| a.dir.cmp(&b.dir));
    crates
}

/// Module path of `rel` (a path inside its crate) truncated to `depth`
/// components. Rust files use `::` and drop `src/` and `mod`/`lib`/`main`
/// stems; other files use their directory path.
fn module_path(rel: &str, depth: usize) -> String {
    let is_rust = rel.ends_with(".rs");
    let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first() == Some(&"src") && parts.len() > 1 {
        parts.remove(0);
    }
    let file = parts.pop().unwrap_or("");
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let mut comps: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
    if is_rust && !matches!(stem, "mod" | "lib" | "main") {
        comps.push(stem.to_string());
    }
    if comps.is_empty() {
        return ROOT_MODULE.to_string();
    }
    comps.truncate(depth.max(1));
    comps.join(if is_rust { "::" } else { "/" })
}

/// `(crate name, module path)` for an indexed file.
pub fn group_for_file(file: &str, crates: &[CrateRoot], depth: usize) -> (String, String) {
    let owner = crates
        .iter()
        .filter(|c| {
            c.dir.is_empty()
                || file
                    .strip_prefix(c.dir.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|c| c.dir.len());
    match owner {
        Some(c) => {
            let rel = if c.dir.is_empty() {
                file
            } else {
                &file[c.dir.len() + 1..]
            };
            (c.name.clone(), module_path(rel, depth))
        }
        None => (NO_CRATE.to_string(), module_path(file, depth)),
    }
}

fn module_key(krate: &str, module: &str) -> String {
    format!("{krate}::{module}")
}

/// Build the overview. `crates` come from [`discover_crates`] (a pure input
/// so the grouping is testable without a filesystem).
pub fn build_overview(
    conn: &Connection,
    project_id: i64,
    crates: &[CrateRoot],
    depth: usize,
) -> anyhow::Result<ArchOverview> {
    // Symbols per (crate, module) and name -> defining groups.
    let mut stmt = conn.prepare("SELECT name, kind, file FROM symbols WHERE project_id = ?1")?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut group_cache: HashMap<String, (String, String)> = HashMap::new();
    let mut per_module: BTreeMap<(String, String), (usize, HashSet<String>)> = BTreeMap::new();
    let mut defs: HashMap<String, (HashSet<String>, HashSet<String>)> = HashMap::new();
    let mut top_level: BTreeMap<String, (usize, HashSet<String>)> = BTreeMap::new();

    for row in rows {
        let (name, kind, file) = row?;
        let (krate, module) = group_cache
            .entry(file.clone())
            .or_insert_with(|| group_for_file(&file, crates, depth))
            .clone();
        let entry = per_module
            .entry((krate.clone(), module.clone()))
            .or_default();
        entry.0 += 1;
        entry.1.insert(kind.clone());
        let d = defs.entry(name).or_default();
        d.0.insert(module_key(&krate, &module));
        d.1.insert(krate);

        let top = file.split('/').next().unwrap_or(&file).to_string();
        let t = top_level.entry(top).or_default();
        t.0 += 1;
        t.1.insert(kind);
    }

    let mut legacy: Vec<ModuleInfo> = top_level
        .into_iter()
        .map(|(name, (symbol_count, kinds))| ModuleInfo {
            name,
            symbol_count,
            edge_types: kinds.len(),
        })
        .collect();
    legacy.sort_by(|a, b| {
        b.symbol_count
            .cmp(&a.symbol_count)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Crates with their modules.
    let mut by_crate: BTreeMap<String, Vec<ModuleInfo>> = BTreeMap::new();
    for ((krate, module), (count, kinds)) in per_module {
        by_crate.entry(krate).or_default().push(ModuleInfo {
            name: module,
            symbol_count: count,
            edge_types: kinds.len(),
        });
    }
    let mut crate_groups: Vec<CrateGroup> = by_crate
        .into_iter()
        .map(|(name, mut modules)| {
            modules.sort_by(|a, b| {
                b.symbol_count
                    .cmp(&a.symbol_count)
                    .then_with(|| a.name.cmp(&b.name))
            });
            let dir = crates
                .iter()
                .find(|c| c.name == name)
                .map(|c| {
                    if c.dir.is_empty() {
                        ".".into()
                    } else {
                        c.dir.clone()
                    }
                })
                .unwrap_or_default();
            CrateGroup {
                symbol_count: modules.iter().map(|m| m.symbol_count).sum(),
                name,
                dir,
                modules,
            }
        })
        .collect();
    crate_groups.sort_by(|a, b| {
        b.symbol_count
            .cmp(&a.symbol_count)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Call edges between groups. A callee only counts when its name is
    // defined in exactly one module (resp. crate).
    let mut stmt = conn.prepare(
        "SELECT file, callee, COUNT(*) FROM call_graph
         WHERE project_id = ?1 GROUP BY file, callee",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? as usize,
        ))
    })?;

    let mut crate_edges: HashMap<(String, String), usize> = HashMap::new();
    let mut module_edges: HashMap<(String, String), usize> = HashMap::new();
    let mut unresolved = 0usize;
    for row in rows {
        let (file, callee, count) = row?;
        let (from_crate, from_module) = group_cache
            .entry(file.clone())
            .or_insert_with(|| group_for_file(&file, crates, depth))
            .clone();
        // Generic std names (`new`, `map`, ...) would be pinned on whichever
        // module happens to define a same-named symbol.
        let defined = defs
            .get(&callee)
            .filter(|_| !super::std_names::is_std_method(&callee));
        let Some((def_modules, def_crates)) = defined else {
            unresolved += count;
            continue;
        };
        let mut resolved = false;
        if def_crates.len() == 1 {
            resolved = true;
            let to_crate = def_crates.iter().next().cloned().unwrap_or_default();
            if to_crate != from_crate {
                *crate_edges
                    .entry((from_crate.clone(), to_crate))
                    .or_default() += count;
            }
        }
        if def_modules.len() == 1 {
            resolved = true;
            let to = def_modules.iter().next().cloned().unwrap_or_default();
            let from = module_key(&from_crate, &from_module);
            if to != from {
                *module_edges.entry((from, to)).or_default() += count;
            }
        }
        if !resolved {
            unresolved += count;
        }
    }

    let to_sorted = |m: HashMap<(String, String), usize>| -> Vec<GroupEdge> {
        let mut v: Vec<GroupEdge> = m
            .into_iter()
            .map(|((from, to), count)| GroupEdge { from, to, count })
            .collect();
        v.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.from.cmp(&b.from))
                .then_with(|| a.to.cmp(&b.to))
        });
        v
    };

    Ok(ArchOverview {
        modules: legacy,
        edge_counts: super::graph::get_edge_kind_counts(conn, project_id),
        crates: crate_groups,
        crate_edges: to_sorted(crate_edges),
        module_edges: to_sorted(module_edges),
        unresolved_calls: unresolved,
    })
}

/// Architecture overview for a project: discovers crates under the project
/// root and groups symbols by crate, then module path (`depth` components).
pub fn arch_overview(
    conn: &Connection,
    project_id: i64,
    depth: usize,
) -> anyhow::Result<ArchOverview> {
    let root: Option<String> = conn
        .query_row(
            "SELECT root_path FROM projects WHERE id = ?1",
            rusqlite::params![project_id],
            |row| row.get(0),
        )
        .ok();
    let mut stmt = conn.prepare("SELECT DISTINCT file FROM symbols WHERE project_id = ?1")?;
    let files: Vec<String> = stmt
        .query_map(rusqlite::params![project_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    let crates = match root {
        Some(r) => discover_crates(Path::new(&r), &files),
        None => Vec::new(),
    };
    build_overview(conn, project_id, &crates, depth)
}

/// Render the overview as plain text (no colors).
pub fn format_text(o: &ArchOverview, max_module_edges: usize) -> String {
    let mut out = Vec::new();
    out.push(format!(
        "  {:<34} {:>7}  {:>5}",
        "CRATE / MODULE", "SYMBOLS", "KINDS"
    ));
    out.push(format!("  {}", "─".repeat(50)));
    for c in &o.crates {
        out.push(format!(
            "  {:<34} {:>7}",
            format!("{} ({})", c.name, c.dir),
            c.symbol_count
        ));
        for m in &c.modules {
            out.push(format!(
                "    {:<32} {:>7}  {:>5}",
                m.name, m.symbol_count, m.edge_types
            ));
        }
    }

    if !o.crate_edges.is_empty() {
        out.push(String::new());
        out.push("  CALLS BETWEEN CRATES".to_string());
        out.push(format!("  {}", "─".repeat(50)));
        for e in &o.crate_edges {
            out.push(format!("  {} -> {}  {}", e.from, e.to, e.count));
        }
    }
    if !o.module_edges.is_empty() {
        out.push(String::new());
        let shown = o.module_edges.len().min(max_module_edges);
        out.push(format!(
            "  CALLS BETWEEN MODULES (top {shown} of {})",
            o.module_edges.len()
        ));
        out.push(format!("  {}", "─".repeat(50)));
        for e in o.module_edges.iter().take(max_module_edges) {
            out.push(format!("  {} -> {}  {}", e.from, e.to, e.count));
        }
    }
    if o.unresolved_calls > 0 {
        out.push(String::new());
        out.push(format!(
            "  {} calls not attributed (std/external or ambiguous callee names)",
            o.unresolved_calls
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::graph::{CallEdge, store_edges};

    fn mem_conn() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::index::schema::run_migrations(&conn).unwrap();
        let pid = crate::index::schema::get_or_create_project(&conn, "/tmp/arch-test").unwrap();
        (conn, pid)
    }

    fn sym(conn: &Connection, pid: i64, name: &str, file: &str) {
        conn.execute(
            "INSERT INTO symbols (name, kind, file, line, language, project_id)
             VALUES (?1, 'function', ?2, 1, 'rust', ?3)",
            rusqlite::params![name, file, pid],
        )
        .unwrap();
    }

    fn call(caller_file: &str, callee: &str) -> CallEdge {
        use std::sync::atomic::{AtomicU32, Ordering};
        // Distinct lines so repeated calls are stored as separate rows.
        static LINE: AtomicU32 = AtomicU32::new(1);
        CallEdge {
            caller: "caller".into(),
            callee: callee.into(),
            file: caller_file.into(),
            line: LINE.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn workspace() -> Vec<CrateRoot> {
        vec![
            CrateRoot {
                dir: "crates/core".into(),
                name: "app-core".into(),
            },
            CrateRoot {
                dir: "crates/cli".into(),
                name: "app-cli".into(),
            },
        ]
    }

    #[test]
    fn module_path_rust_files() {
        assert_eq!(module_path("src/index/graph.rs", 2), "index::graph");
        assert_eq!(module_path("src/index/mod.rs", 2), "index");
        assert_eq!(module_path("src/main.rs", 2), ROOT_MODULE);
        assert_eq!(
            module_path("src/engine/rules/builtin.rs", 2),
            "engine::rules"
        );
        assert_eq!(module_path("tests/cli.rs", 2), "tests::cli");
    }

    #[test]
    fn module_path_other_languages_use_directories() {
        assert_eq!(module_path("web/ui/button.ts", 2), "web/ui");
        assert_eq!(module_path("setup.py", 2), ROOT_MODULE);
    }

    #[test]
    fn group_for_file_picks_nearest_crate() {
        let mut crates = workspace();
        crates.push(CrateRoot {
            dir: "crates/core/inner".into(),
            name: "inner".into(),
        });
        let g = |f: &str| group_for_file(f, &crates, 2);
        assert_eq!(
            g("crates/core/src/db/mod.rs"),
            ("app-core".into(), "db".into())
        );
        assert_eq!(
            g("crates/cli/src/main.rs"),
            ("app-cli".into(), ROOT_MODULE.into())
        );
        assert_eq!(
            g("crates/core/inner/src/lib.rs"),
            ("inner".into(), ROOT_MODULE.into())
        );
        // Same name prefix but a different directory must not match.
        assert_eq!(g("crates/core2/src/lib.rs").0, NO_CRATE);
        assert_eq!(g("scripts/build.py").0, NO_CRATE);
    }

    #[test]
    fn overview_groups_by_crate_then_module_with_edge_counts() {
        let (conn, pid) = mem_conn();
        sym(&conn, pid, "open_db", "crates/core/src/db/mod.rs");
        sym(&conn, pid, "query_rows", "crates/core/src/db/query.rs");
        sym(&conn, pid, "run_cli", "crates/cli/src/main.rs");
        sym(&conn, pid, "parse_args", "crates/cli/src/args.rs");
        store_edges(
            &conn,
            &[
                call("crates/cli/src/main.rs", "open_db"),
                call("crates/cli/src/main.rs", "open_db"),
                call("crates/cli/src/args.rs", "query_rows"),
                call("crates/cli/src/main.rs", "parse_args"),
                call("crates/cli/src/main.rs", "unwrap"),
            ],
            pid,
        )
        .unwrap();

        let o = build_overview(&conn, pid, &workspace(), 2).unwrap();

        let names: Vec<&str> = o.crates.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["app-cli", "app-core"]);
        let core = o.crates.iter().find(|c| c.name == "app-core").unwrap();
        assert_eq!(core.symbol_count, 2);
        let mods: Vec<&str> = core.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(mods, vec!["db", "db::query"]);

        // cli -> core: open_db x2 + query_rows x1 (parse_args stays inside cli).
        assert_eq!(
            o.crate_edges,
            vec![GroupEdge {
                from: "app-cli".into(),
                to: "app-core".into(),
                count: 3
            }]
        );
        let edge = |from: &str, to: &str| {
            o.module_edges
                .iter()
                .find(|e| e.from == from && e.to == to)
                .map(|e| e.count)
        };
        assert_eq!(edge("app-cli::(root)", "app-core::db"), Some(2));
        assert_eq!(edge("app-cli::args", "app-core::db::query"), Some(1));
        assert_eq!(edge("app-cli::(root)", "app-cli::args"), Some(1));
        // `unwrap` is defined nowhere in the project.
        assert_eq!(o.unresolved_calls, 1);
    }

    #[test]
    fn ambiguous_callee_names_are_not_attributed() {
        let (conn, pid) = mem_conn();
        sym(&conn, pid, "new", "crates/core/src/a.rs");
        sym(&conn, pid, "new", "crates/cli/src/b.rs");
        sym(&conn, pid, "main", "crates/cli/src/main.rs");
        store_edges(&conn, &[call("crates/cli/src/main.rs", "new")], pid).unwrap();
        let o = build_overview(&conn, pid, &workspace(), 2).unwrap();
        assert!(o.crate_edges.is_empty());
        assert!(o.module_edges.is_empty());
        assert_eq!(o.unresolved_calls, 1);
    }

    #[test]
    fn discover_crates_reads_package_names_from_disk() {
        let dir = std::env::temp_dir().join(format!("cora-arch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::create_dir_all(dir.join("crates/b/src")).unwrap();
        // Workspace root without [package] is not a crate.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("crates/a/Cargo.toml"),
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("crates/b/Cargo.toml"),
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let files = vec![
            "crates/a/src/lib.rs".to_string(),
            "crates/b/src/lib.rs".to_string(),
            "README.md".to_string(),
        ];
        let found = discover_crates(&dir, &files);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            found,
            vec![
                CrateRoot {
                    dir: "crates/a".into(),
                    name: "alpha".into()
                },
                CrateRoot {
                    dir: "crates/b".into(),
                    name: "beta".into()
                },
            ]
        );
    }

    #[test]
    fn text_output_lists_crates_before_modules() {
        let (conn, pid) = mem_conn();
        sym(&conn, pid, "open_db", "crates/core/src/db/mod.rs");
        sym(&conn, pid, "run_cli", "crates/cli/src/main.rs");
        store_edges(&conn, &[call("crates/cli/src/main.rs", "open_db")], pid).unwrap();
        let o = build_overview(&conn, pid, &workspace(), 2).unwrap();
        let text = format_text(&o, 10);
        let cli = text.find("app-cli").unwrap();
        let core = text.find("app-core (crates/core)").unwrap();
        assert!(cli < text.find("(root)").unwrap());
        assert!(core > cli);
        assert!(text.contains("CALLS BETWEEN CRATES"));
        assert!(text.contains("app-cli -> app-core  1"));
    }

    #[test]
    fn arch_overview_without_cargo_manifests_groups_by_directory() {
        let (conn, pid) = mem_conn();
        sym(&conn, pid, "main", "src/main.rs");
        let o = arch_overview(&conn, pid, 2).unwrap();
        assert_eq!(o.crates.len(), 1);
        assert_eq!(o.crates[0].name, NO_CRATE);
        assert_eq!(o.modules[0].name, "src");
    }

    #[test]
    fn std_method_names_are_not_attributed_to_a_module() {
        let (conn, pid) = mem_conn();
        // A project-local `map` must not soak up every `.map(...)` call.
        sym(&conn, pid, "map", "crates/core/src/db/mod.rs");
        sym(&conn, pid, "run_cli", "crates/cli/src/main.rs");
        store_edges(&conn, &[call("crates/cli/src/main.rs", "map")], pid).unwrap();
        let o = build_overview(&conn, pid, &workspace(), 2).unwrap();
        assert!(o.crate_edges.is_empty());
        assert!(o.module_edges.is_empty());
        assert_eq!(o.unresolved_calls, 1);
    }
}
