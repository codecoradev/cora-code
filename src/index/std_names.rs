//! Well-known `std` / `core` method names.
//!
//! These dominate a name-based call graph (`unwrap`, `clone`, `iter`, ...)
//! and carry no architectural signal, so `cora query` hides them by default
//! and `cora arch` does not attribute them to a module.

/// Method names from `std` / `core` that dominate call-graph output
/// (`unwrap`, `clone`, `iter`, ...). Sorted for binary search.
///
/// A name is only hidden when the project does not define a symbol with the
/// same name (see `commands::query`), so a project's own `new` or `read` is
/// still shown.
pub const STD_METHOD_NAMES: &[&str] = &[
    "all",
    "and_then",
    "any",
    "as_bytes",
    "as_deref",
    "as_mut",
    "as_ref",
    "as_str",
    "borrow",
    "borrow_mut",
    "chain",
    "chars",
    "clear",
    "clone",
    "cloned",
    "cmp",
    "collect",
    "contains",
    "contains_key",
    "copied",
    "count",
    "default",
    "deref",
    "display",
    "drop",
    "ends_with",
    "entry",
    "enumerate",
    "eq",
    "expect",
    "extend",
    "filter",
    "filter_map",
    "find",
    "first",
    "flat_map",
    "flatten",
    "flush",
    "fmt",
    "fold",
    "for_each",
    "format",
    "from",
    "get",
    "get_mut",
    "hash",
    "insert",
    "into",
    "into_iter",
    "is_empty",
    "is_err",
    "is_none",
    "is_ok",
    "is_some",
    "iter",
    "iter_mut",
    "join",
    "keys",
    "last",
    "len",
    "lines",
    "lock",
    "map",
    "map_err",
    "max",
    "min",
    "new",
    "next",
    "nth",
    "ok",
    "ok_or",
    "ok_or_else",
    "or_default",
    "or_else",
    "or_insert",
    "or_insert_with",
    "parent",
    "parse",
    "peekable",
    "pop",
    "position",
    "push",
    "push_str",
    "remove",
    "replace",
    "retain",
    "rev",
    "skip",
    "sort",
    "sort_by",
    "sort_by_key",
    "split",
    "split_once",
    "starts_with",
    "strip_prefix",
    "strip_suffix",
    "sum",
    "take",
    "to_lowercase",
    "to_owned",
    "to_path_buf",
    "to_string",
    "to_uppercase",
    "to_vec",
    "trim",
    "truncate",
    "try_from",
    "try_into",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "values",
    "with_capacity",
    "write_all",
    "zip",
];

/// True when `name` (optionally `Type::name`) is a well-known std/core method.
pub fn is_std_method(name: &str) -> bool {
    let bare = name.rsplit("::").next().unwrap_or(name);
    STD_METHOD_NAMES.binary_search(&bare).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_method_list_is_sorted_and_unique() {
        for w in STD_METHOD_NAMES.windows(2) {
            assert!(w[0] < w[1], "{} must sort before {}", w[0], w[1]);
        }
    }

    #[test]
    fn is_std_method_handles_qualified_names() {
        assert!(is_std_method("unwrap"));
        assert!(is_std_method("Vec::new"));
        assert!(!is_std_method("authenticate"));
    }
}
