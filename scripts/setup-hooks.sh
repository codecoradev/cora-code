#!/usr/bin/env bash
# Point git at the committed hooks in scripts/hooks (once per clone).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
chmod +x scripts/hooks/pre-commit
git config core.hooksPath scripts/hooks
echo "git hooks now come from scripts/hooks"
