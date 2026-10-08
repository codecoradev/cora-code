//! The single path-pattern matcher for ignore/skip/include/exclude lists.
//!
//! Before this module existed two matchers disagreed on the same config:
//! a hand-rolled `should_skip_file` (index time, review-time index scanners)
//! and `glob::Pattern` (`cora scan` include/exclude, `cora watch --filter`).
//!
//! Semantics (a pattern matches a `/`-separated, project-relative path when
//! ANY of the following holds):
//!
//! 1. the pattern equals the whole path or the basename (`src/main.ts`,
//!    `main.ts`);
//! 2. the pattern, as a glob, matches the whole path. `*` also crosses `/`
//!    (the `glob` crate default, which `scan`/`watch` already relied on) and
//!    `**/` matches zero or more directories;
//! 3. the pattern, after dropping any leading `**/`, has no `/` and, as a glob,
//!    matches the basename (`*.config.ts`, `vite.config.*`, `**/*.test.ts`);
//! 4. the pattern ends in `/**` and the part before it matches the path itself
//!    (`src/engine/**` also skips a file literally named `src/engine`,
//!    `**/phaser/**` also matches `a/phaser`).
//!
//! Known, intentional differences from the retired hand-rolled matcher:
//! `**/*.test.ts` no longer matches `footest.ts` (it compared against the
//! extension without its dot), `vite.config.*` no longer matches
//! `vite.configx`, and wildcard patterns such as `src/*.rs` now match instead
//! of being silently ignored. Patterns that fail to compile as globs fall
//! back to rule 1 only. The differences from plain `glob::Pattern` are rules 1,
//! 3 and 4: slash-free patterns also match by basename, so
//! `cora scan --exclude 'vite.config.*'` now also excludes nested configs.

use glob::Pattern;

/// One compiled pattern.
#[derive(Debug, Clone)]
pub struct PathPattern {
    raw: String,
    full: Option<Pattern>,
    /// Basename-only glob (rule 3), present for slash-free patterns.
    base: Option<Pattern>,
    /// Glob for the directory itself (rule 4), present for `dir/**` patterns.
    dir: Option<Pattern>,
}

impl PathPattern {
    /// Compile a pattern. Fails only when the pattern is not a valid glob.
    pub fn new(pattern: &str) -> Result<Self, glob::PatternError> {
        let full = Pattern::new(pattern)?;
        let mut core = pattern;
        while let Some(rest) = core.strip_prefix("**/") {
            core = rest;
        }
        let base = if core.contains('/') {
            None
        } else {
            Pattern::new(core).ok()
        };
        let dir = pattern
            .strip_suffix("/**")
            .filter(|d| !d.is_empty())
            .and_then(|d| Pattern::new(d).ok());
        Ok(Self {
            raw: pattern.to_string(),
            full: Some(full),
            base,
            dir,
        })
    }

    /// Compile leniently: an invalid glob still matches literally (rule 1).
    pub fn lenient(pattern: &str) -> Self {
        Self::new(pattern).unwrap_or_else(|_| Self {
            raw: pattern.to_string(),
            full: None,
            base: None,
            dir: None,
        })
    }

    pub fn matches(&self, path: &str) -> bool {
        if self.raw.is_empty() {
            return false;
        }
        let basename = path.rsplit('/').next().unwrap_or(path);
        if path == self.raw || basename == self.raw {
            return true;
        }
        if self.full.as_ref().is_some_and(|g| g.matches(path)) {
            return true;
        }
        if self.base.as_ref().is_some_and(|g| g.matches(basename)) {
            return true;
        }
        self.dir.as_ref().is_some_and(|g| g.matches(path))
    }
}

/// A set of patterns; a path matches when any pattern does.
#[derive(Debug, Clone, Default)]
pub struct PathMatcher {
    patterns: Vec<PathPattern>,
}

impl PathMatcher {
    /// Compile every pattern (invalid globs degrade to literal matching).
    pub fn new(patterns: &[String]) -> Self {
        Self {
            patterns: patterns.iter().map(|p| PathPattern::lenient(p)).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.patterns.iter().any(|p| p.matches(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pats: &[&str]) -> PathMatcher {
        PathMatcher::new(&pats.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn empty_matcher_matches_nothing() {
        assert!(!m(&[]).is_match("src/main.rs"));
        assert!(m(&[]).is_empty());
        assert!(!m(&[""]).is_match("src/main.rs"));
    }

    #[test]
    fn exact_and_basename() {
        let x = m(&["src/main.ts", "Makefile"]);
        assert!(x.is_match("src/main.ts"));
        assert!(x.is_match("a/b/Makefile"));
        assert!(!x.is_match("src/main.tsx"));
    }

    #[test]
    fn double_star_variants() {
        let x = m(&["**/*.test.ts"]);
        assert!(x.is_match("a.test.ts"));
        assert!(x.is_match("a/b/c.test.ts"));
        assert!(!x.is_match("footest.ts"));
        assert!(!x.is_match("a/c.test.js"));

        let d = m(&["**/phaser/**"]);
        assert!(d.is_match("phaser/a.ts"));
        assert!(d.is_match("src/phaser/a/b.ts"));
        assert!(d.is_match("a/phaser"));
        assert!(!d.is_match("src/phaserHelper.ts"));

        let s = m(&["**/something"]);
        assert!(s.is_match("something"));
        assert!(s.is_match("a/b/something"));
        assert!(!s.is_match("a/b/something-else"));
    }

    #[test]
    fn dir_prefix() {
        let x = m(&["target/**", "src/engine/**"]);
        assert!(x.is_match("target/debug/x.rs"));
        assert!(x.is_match("src/engine/core/mod.rs"));
        assert!(x.is_match("src/engine"));
        assert!(!x.is_match("src/app/engine.rs"));
        assert!(!x.is_match("crates/a/target/x.rs"));
    }

    #[test]
    fn wildcards_and_edge_cases() {
        assert!(m(&["vite.config.*"]).is_match("apps/web/vite.config.ts"));
        assert!(!m(&["vite.config.*"]).is_match("vite.configx"));
        assert!(m(&["src/*.rs"]).is_match("src/lib.rs"));
        assert!(m(&["*.gen.*"]).is_match("a/b/x.gen.go"));
        assert!(!m(&["*.config.ts"]).is_match("config.ts"));
    }

    #[test]
    fn invalid_glob_degrades_to_literal() {
        let x = m(&["a[b"]);
        assert!(x.is_match("a[b"));
        assert!(!x.is_match("ab"));
        assert!(PathPattern::new("a[b").is_err());
    }
}
