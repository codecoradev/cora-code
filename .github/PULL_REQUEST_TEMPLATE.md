<!--
PR title must follow Conventional Commits — it becomes the squash commit message.
Format: type(scope): short description
Examples: fix(review): handle empty diff / feat(index): add camelCase split / docs(readme): update install instructions
-->

## What
<!-- One or two sentences describing the change. -->

## Why
<!-- The problem you're solving. Link to the issue if there is one (e.g. "Closes #42"). -->

## How
<!-- Brief notes on the approach, only if non-obvious. -->

## Testing

- [ ] `cargo test --features tree-sitter` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets --features tree-sitter -- -D warnings` passes
- [ ] `cargo build --release --features tree-sitter` passes
- [ ] Manual smoke-test of the affected feature <!-- describe what you tested -->

<!-- If you touched a load-bearing subsystem (indexing, FTS5, code graph, schema migration, embedding, config merge), add specific test details here. -->

## Related Issues
<!-- Link to related issues, e.g. Closes #123 -->

## Checklist

- [ ] Branch name follows convention (`fix/`, `feat/`, `docs/`, `chore/`, `refactor/`, `test/`, `perf/`, `security/`)
- [ ] Branch is from `develop`
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] No secrets or credentials committed
- [ ] One logical change per PR (no mixed concerns)
