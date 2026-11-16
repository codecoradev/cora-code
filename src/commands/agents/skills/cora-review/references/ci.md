{{CORA_SKILL_MARKER}}

# cora review in CI

A review in CI looks at every pull request the same way, whoever or whatever wrote the change. If the project has none, suggest it (do not add a workflow without asking).

## GitHub Actions

```yaml
# .github/workflows/cora-review.yml
on:
  pull_request_target:
    branches: [main, develop]
    types: [opened, synchronize, ready_for_review, reopened]

permissions:
  contents: read
  pull-requests: write
  security-events: write

jobs:
  cora-review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false
      # The config comes from the base branch, not from the PR (see below).
      - name: Use the base branch's .cora.yaml
        env:
          BASE_SHA: ${{ github.event.pull_request.base.sha }}
        run: git show "$BASE_SHA:.cora.yaml" > .cora.yaml 2>/dev/null || rm -f .cora.yaml
      - uses: codecoradev/cora-review-action@v1
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
          cora-api-key: ${{ secrets.CORA_API_KEY }}
          cora-base-url: ${{ secrets.CORA_BASE_URL }}
          cora-model: ${{ secrets.CORA_MODEL }}
```

The action downloads cora, runs `cora review` on the PR diff, comments on the PR, uploads SARIF to Code Scanning and fails the job on blocking findings. The model, provider and base URL are inputs (`cora-model`, `cora-base-url`); the key is a repository secret, never a file in the repo.

## Any other CI

Run `cora review --base origin/<target-branch> --ci` with `CORA_API_KEY` set as a secret (an LLM judge; `--ci` lifts the diff size limit and exits with 2 when there are findings; add `--format sarif` for Code Scanning), or `cora review --no-llm` for the deterministic checks only. Fetch enough history for the base branch (`fetch-depth: 0` or an explicit fetch). No `cora index` step is needed: cora indexes the checkout itself before building the prompt, and release binaries carry the code embedding model, so it works on a fresh runner.

## Things to get right

- Commit the `.cora.yaml` ignores first (see "Before the first review: ignore" in SKILL.md): CI should not review generated or vendored files.
- `pull_request_target` gives pull requests from forks the secrets, so the job checks out the PR head to read it: never install, build or run anything from that checkout, and keep `persist-credentials: false`.
- cora reads `.cora.yaml` from the checkout, so a pull request can change its own review: `ignore.files` or `review.exclude` to hide files, `provider.base_url` to receive the API key (cora ignores a base URL from an untrusted project config, but pin it anyway with the `cora-base-url` and `cora-model` inputs or `CORA_BASE_URL` and `CORA_MODEL`), prompts and rules. Take the config from the base branch as in the workflow above, and review a change to `.cora.yaml` by hand.
- The log shows `Code index: N symbols; code model on`. `code model off` means only keyword and call-graph context was used.
- More examples (GitLab, Gitea, Bitbucket, pre-commit): the project's `docs/examples.md`.
