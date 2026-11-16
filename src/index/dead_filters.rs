//! Default exclusions for dead-code detection (#716).
//!
//! The symbol table records no attributes or enclosing-impl information, so
//! the rules that need them (`#[test]`, `#[cfg(test)]` modules, trait-impl
//! methods) read the Rust source at query time, only for the few symbols that
//! already survived the SQL filter. Path rules and visibility need no source.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// True for files that hold tests: a `tests`/`test`/`__tests__`/`spec`
/// directory, `tests.rs`, `*_test(s).rs|go`, `*.test.*`, `*.spec.*` and
/// `test_*.py` / `conftest.py`.
pub(crate) fn is_test_path(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let mut parts: Vec<&str> = norm.split('/').collect();
    let file = parts.pop().unwrap_or("");
    if parts
        .iter()
        .any(|d| matches!(*d, "tests" | "test" | "__tests__" | "spec" | "specs"))
    {
        return true;
    }
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (file, ""),
    };
    if matches!(stem, "tests" | "conftest") && matches!(ext, "rs" | "py") {
        return true;
    }
    if ext == "py" && stem.starts_with("test_") {
        return true;
    }
    stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
}

/// Whether a signature declares public API: Rust `pub` (not `pub(crate)`,
/// `pub(super)`, `pub(self)` or `pub(in ..)`) and TS/JS `export`.
pub(crate) fn is_public_signature(sig: &str) -> bool {
    let s = sig.trim_start();
    if s.starts_with("export ") {
        return true;
    }
    if let Some(rest) = s.strip_prefix("pub(") {
        let scope = rest.split(')').next().unwrap_or("");
        return !(scope == "crate"
            || scope == "super"
            || scope == "self"
            || scope.starts_with("in "));
    }
    s.starts_with("pub ")
}

/// Crate root `lib.rs` for a Rust source file (`crates/x/src/a.rs` ->
/// `crates/x/src/lib.rs`); `None` when the path has no `src/` component.
pub(crate) fn lib_root_for(file: &str) -> Option<String> {
    let idx = if file.starts_with("src/") {
        0
    } else {
        file.find("/src/")? + 1
    };
    Some(format!("{}src/lib.rs", &file[..idx]))
}

/// Lazily read source lines, one read per file.
pub(crate) struct SourceCache {
    root: PathBuf,
    files: HashMap<String, Option<Vec<String>>>,
}

impl SourceCache {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: HashMap::new(),
        }
    }

    fn lines(&mut self, file: &str) -> Option<&Vec<String>> {
        let root = &self.root;
        self.files
            .entry(file.to_string())
            .or_insert_with(|| {
                std::fs::read_to_string(root.join(file))
                    .ok()
                    .map(|s| s.lines().map(str::to_string).collect())
            })
            .as_ref()
    }

    /// Whether the fn defined at 1-based `line` of `file` is test-only code or
    /// a trait method. `None` when the source cannot be read.
    pub(crate) fn rust_context(&mut self, file: &str, line: u32) -> Option<RustContext> {
        let lines = self.lines(file)?;
        Some(rust_context(lines, line))
    }
}

/// What the source says about a Rust fn.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RustContext {
    /// `#[test]`-like attribute, `#[cfg(test)]` item, or inside a test module.
    pub is_test: bool,
    /// Method of `impl Trait for Type` or of a `trait` block: called through
    /// dynamic or generic dispatch the call graph cannot see.
    pub is_trait_method: bool,
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn first_token(t: &str) -> &str {
    let t = t.trim_start();
    let end = t
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(t.len());
    &t[..end]
}

/// Header keyword of an item line (`pub(crate) unsafe impl<T> ..` -> `impl`).
fn item_keyword(line: &str) -> Option<&'static str> {
    let mut t = line.trim_start();
    for _ in 0..4 {
        let tok = first_token(t);
        match tok {
            "pub" => {
                t = t[3..].trim_start();
                if t.starts_with('(') {
                    t = t.split_once(')').map_or("", |x| x.1).trim_start();
                }
            }
            "unsafe" | "async" | "const" | "default" => t = t[tok.len()..].trim_start(),
            "impl" => return Some("impl"),
            "trait" => return Some("trait"),
            "mod" => return Some("mod"),
            "fn" => return Some("fn"),
            "struct" | "enum" | "union" => return Some("type"),
            _ => return None,
        }
    }
    None
}

fn is_attr_or_doc(t: &str) -> bool {
    t.starts_with("#[") || t.starts_with("#![") || t.starts_with("//")
}

fn attrs_above(lines: &[String], start: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = start;
    while i > 0 {
        let t = lines[i - 1].trim();
        if !is_attr_or_doc(t) {
            break;
        }
        if t.starts_with('#') {
            out.push(t);
        }
        i -= 1;
    }
    out
}

fn attr_marks_test(attr: &str) -> bool {
    let a = attr.replace(' ', "");
    a.contains("cfg(test)")
        || a == "#[test]"
        || a.ends_with("::test]")
        || a.contains("::test(")
        || a.starts_with("#[test(")
        || a.starts_with("#[rstest")
        || a.starts_with("#[bench")
        || a.starts_with("#[wasm_bindgen_test")
}

fn rust_context(lines: &[String], line: u32) -> RustContext {
    let mut ctx = RustContext::default();
    let idx = (line as usize).saturating_sub(1);
    if idx >= lines.len() {
        return ctx;
    }
    if attrs_above(lines, idx).iter().any(|a| attr_marks_test(a)) {
        ctx.is_test = true;
    }

    // Climb the enclosing blocks by indentation: the first less-indented line
    // above is the opener of the block holding the current line.
    let mut cur = indent(&lines[idx]);
    let mut i = idx;
    while cur > 0 && i > 0 {
        i -= 1;
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with("//") || indent(&lines[i]) >= cur {
            continue;
        }
        // Raw-string fixtures and closers are not block openers.
        if !t.ends_with('{') {
            continue;
        }
        // The header may span lines (`impl<T> Tr for X<T>` / `where` / `{`).
        let mut j = i;
        while j > 0 && item_keyword(&lines[j]).is_none() && i - j < 12 {
            j -= 1;
        }
        let Some(kw) = item_keyword(&lines[j]) else {
            cur = indent(&lines[i]);
            continue;
        };
        let header = lines[j..=i].join(" ");
        let header = header.split('{').next().unwrap_or("");
        match kw {
            "impl" if header.contains(" for ") => ctx.is_trait_method = true,
            "trait" => ctx.is_trait_method = true,
            "mod" => {
                let name = header
                    .split("mod")
                    .nth(1)
                    .map(|s| first_token(s).to_string())
                    .unwrap_or_default();
                if matches!(name.as_str(), "tests" | "test")
                    || attrs_above(lines, j).iter().any(|a| attr_marks_test(a))
                {
                    ctx.is_test = true;
                }
            }
            _ => {}
        }
        if kw != "mod"
            && attrs_above(lines, j)
                .iter()
                .any(|a| a.contains("cfg(test)"))
        {
            ctx.is_test = true;
        }
        cur = indent(&lines[j]);
        i = j;
    }
    ctx
}

/// Set of project file paths, for lib-crate detection.
pub(crate) fn load_known_files(
    conn: &rusqlite::Connection,
    project_id: i64,
) -> anyhow::Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT path FROM files WHERE project_id = ?1
         UNION SELECT DISTINCT file FROM symbols WHERE project_id = ?1",
    )?;
    let rows = stmt.query_map([project_id], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ls(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    #[test]
    fn test_paths() {
        for p in [
            "tests/a.rs",
            "crates/x/tests/b.rs",
            "src/engine/tests.rs",
            "src/foo_tests.rs",
            "src/foo_test.go",
            "web/a.test.ts",
            "web/a.spec.tsx",
            "pkg/test_a.py",
            "__tests__/a.js",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        // Regression: engine code that merely has "test" in its name.
        for p in [
            "src/engine/test_context.rs",
            "src/testing_utils.rs",
            "src/contest.rs",
            "src/latest/a.rs",
        ] {
            assert!(!is_test_path(p), "{p}");
        }
    }

    #[test]
    fn public_signatures() {
        assert!(is_public_signature("pub fn a()"));
        assert!(is_public_signature("pub async fn a()"));
        assert!(is_public_signature("export function a()"));
        assert!(!is_public_signature("pub(in crate::x) fn a()"));
        assert!(!is_public_signature("pub(crate) fn a()"));
        assert!(!is_public_signature("pub(super) fn a()"));
        assert!(!is_public_signature("fn a()"));
        assert!(!is_public_signature("pubfn a()"));
    }

    #[test]
    fn lib_roots() {
        assert_eq!(lib_root_for("src/a.rs").as_deref(), Some("src/lib.rs"));
        assert_eq!(
            lib_root_for("crates/x/src/a/b.rs").as_deref(),
            Some("crates/x/src/lib.rs")
        );
        assert_eq!(lib_root_for("build.rs"), None);
    }

    #[test]
    fn test_attribute_and_cfg_test_module() {
        let src = "\
fn plain() {}

#[test]
fn unit() {}

#[tokio::test]
async fn async_unit() {}

#[cfg(test)]
mod tests {
    fn helper() {}

    mod nested {
        fn deep() {}
    }
}
";
        let l = ls(src);
        let at = |n: &str| {
            let line = l
                .iter()
                .position(|x| x.contains(&format!("fn {n}")))
                .unwrap();
            rust_context(&l, line as u32 + 1)
        };
        assert!(!at("plain").is_test);
        assert!(at("unit").is_test);
        assert!(at("async_unit").is_test);
        assert!(at("helper").is_test, "helper in #[cfg(test)] mod");
        assert!(at("deep").is_test, "nested module inside cfg(test)");
    }

    #[test]
    fn trait_impl_methods() {
        let src = "\
struct S;
impl S {
    fn inherent(&self) {}
}
impl std::fmt::Display for S {
    fn shown(&self) {}
}
impl<T> Handler<T> for S
where
    T: Clone,
{
    fn multi_line_header(&self) {}
}
trait Shape {
    fn area(&self) -> u32 { 0 }
}
";
        let l = ls(src);
        let at = |n: &str| {
            let line = l
                .iter()
                .position(|x| x.contains(&format!("fn {n}")))
                .unwrap();
            rust_context(&l, line as u32 + 1)
        };
        assert!(!at("inherent").is_trait_method);
        assert!(at("shown").is_trait_method);
        assert!(at("multi_line_header").is_trait_method);
        assert!(at("area").is_trait_method);
    }

    /// Regression: a `for` inside generics or a nested closure block must not
    /// make an inherent impl look like a trait impl.
    #[test]
    fn inherent_impl_with_for_loop_is_not_a_trait_impl() {
        let src = "\
impl Runner {
    fn go(&self) {
        for x in 0..3 {
            let f = || {
                fn inner() {}
            };
        }
    }
}
";
        let l = ls(src);
        let line = l.iter().position(|x| x.contains("fn inner")).unwrap();
        let c = rust_context(&l, line as u32 + 1);
        assert!(!c.is_trait_method);
        assert!(!c.is_test);
    }
}
