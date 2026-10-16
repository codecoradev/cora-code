/// Pre-commit hook template content (v2, #674).
///
/// 1. A staged diff that an agent (`cora.review_finalize`) or `cora review
///    --staged` already judged passes at once: `cora hook verify` exits 0.
/// 2. Otherwise the hook runs `cora review --staged --ci --severity major`
///    (an LLM when a key is configured, the deterministic checks otherwise).
///
/// `cora review` exit codes: 0 = pass, 2 = blocking findings (deny the commit),
/// anything else = tooling error (the commit proceeds, but it is NOT a pass).
/// `cora hook verify` exit codes: 0 validated, 10 validated but blocking,
/// 11 not validated; an older cora without the subcommand exits 2 (a usage
/// error), which falls through to the full review.
pub const HOOK_TEMPLATE: &str = r#"#!/usr/bin/env bash
# cora-managed-hook
# cora pre-commit hook (v2) — installed by `cora hook install`.
# Run `cora hook uninstall` to remove.

set -uo pipefail

# Locate cora binary (prefer the one used to install the hook)
CORA_BIN="${CORA_BIN:-cora}"

# 1. The staged diff was already reviewed (by an agent or by `cora review --staged`).
"$CORA_BIN" hook verify --quiet
case $? in
    0)
        echo "✅ cora: staged diff already validated."
        exit 0
        ;;
    10)
        echo "❌ cora: the staged diff was reviewed and the verdict blocks the commit."
        echo "   Fix the findings, git add, and review again (see: cora hook verify)."
        exit 2
        ;;
esac
# 11 = not validated; any other code (an older cora) falls through to a full review.

# 2. Fallback: review the staged diff now.
echo "🔍 Running cora review..."

# Update the symbol index (incremental — fast, non-blocking)
"$CORA_BIN" index >/dev/null 2>&1 || true

"$CORA_BIN" review --staged --ci --severity major --format compact
code=$?
case $code in
    0)
        echo "✅ cora review passed."
        exit 0
        ;;
    2)
        echo "❌ cora review found blocking issues. Commit denied."
        echo "   Fix the issues above; do not use --no-verify to get past a finding."
        exit 2
        ;;
    *)
        echo "⚠️  cora review did not complete (exit $code): that is not a pass."
        echo "   Re-run: cora review --base <default-branch> --ci --severity major"
        echo "   and get a real verdict before pushing."
        exit 0
        ;;
esac
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_a_managed_v2_hook_with_both_tiers() {
        assert!(HOOK_TEMPLATE.starts_with("#!/usr/bin/env bash\n# cora-managed-hook\n"));
        assert!(HOOK_TEMPLATE.contains("hook verify --quiet"));
        assert!(HOOK_TEMPLATE.contains("review --staged --ci --severity major"));
    }

    #[test]
    fn template_denies_only_on_exit_2_or_10() {
        // The tooling-error branch must not deny (and must say it is no pass).
        assert!(HOOK_TEMPLATE.contains("that is not a pass"));
        assert_eq!(HOOK_TEMPLATE.matches("exit 2").count(), 2);
        // An old cora (usage error 2 from `hook verify`) falls through.
        assert!(HOOK_TEMPLATE.contains("falls through to a full review"));
    }

    #[test]
    fn template_never_tells_users_to_bypass_a_finding() {
        assert!(HOOK_TEMPLATE.contains("do not use --no-verify to get past a finding"));
        assert!(!HOOK_TEMPLATE.contains("or run: git commit --no-verify"));
    }
}
