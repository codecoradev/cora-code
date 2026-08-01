/// Pre-commit hook template content.
///
/// The hook runs `cora index --quiet` (incremental, ~0.014s) then
/// `cora review --staged --format compact` and checks the exit
/// code: 0 = ok (allow commit), 1 = error (allow commit), 2 = blocked (deny commit).
pub const HOOK_TEMPLATE: &str = r#"#!/usr/bin/env bash
# cora pre-commit hook — installed by `cora hook install`
# Run `cora hook uninstall` to remove.

set -euo pipefail

# Locate cora binary (prefer the one used to install the hook)
CORA_BIN="${CORA_BIN:-cora}"

echo "🔍 Running cora code review..."

# Update the symbol index (incremental — fast, non-blocking)
"$CORA_BIN" index 2>/dev/null || true

# Run cora review on staged changes (uses index for brain enrichment)
if "$CORA_BIN" review --staged --format compact 2>/dev/null; then
    echo "✅ cora review passed."
    exit 0
else
    EXIT_CODE=$?
    if [ $EXIT_CODE -eq 2 ]; then
        echo "❌ cora review found blocking issues. Commit denied."
        echo "   Fix the issues above, or run: git commit --no-verify"
        exit 2
    elif [ $EXIT_CODE -eq 1 ]; then
        echo "⚠️  cora review encountered an error (commit will proceed)."
        exit 0
    else
        echo "⚠️  cora exited with code $EXIT_CODE (commit will proceed)."
        exit 0
    fi
fi
"#;
