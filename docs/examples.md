---
title: Examples
---

# Examples

Practical examples to get you started with cora.

## 01 — Quick Review

Review your staged changes before committing.

```bash
# Review staged changes (default)
$ cora review

# Or review with explicit flags
$ cora review --staged
```

## 02 — Branch Comparison

Compare your current branch against main.

```bash
$ cora review --base main
```

## 03 — Full Project Scan

Scan your entire project for issues.

```bash
$ cora scan
```

## 04 — Incremental Scan

Only scan files that changed since the last scan.

```bash
$ cora scan --incremental
```

## 05 — Streaming Output

Stream results as they come in from the LLM.

```bash
$ cora review --staged --stream
```

## 06 — Auto-Commit (Review + Message + Commit)

Review staged changes, generate a conventional commit message, and commit in one step.

### HITL mode (default — interactive prompt)

```bash
$ git add -A
$ cora commit
🔍 Reviewing staged changes (3 files, 247 lines)...
✅ No issues found (quality score: 9.2/10)

📝 Generated commit message:
  feat(auth): add session expiry validation

Accept commit message? [Y]es / [E]dit / [N]o › y
✅ Committed: feat(auth): add session expiry validation
```

### YOLO mode (CI / trusted workflows)

```bash
$ cora commit --yolo
```

### Skip review (message only)

```bash
$ cora commit --no-review
```

### Force commit past quality gate

```bash
$ cora commit --force
```

## 06 — GitHub Actions CI (Recommended)

The easiest way to add cora to your PR workflow. This reusable action installs cora, runs the review, posts a PR comment with findings, and optionally uploads SARIF to GitHub Code Scanning.

### Setup

1. Add these secrets to your repository (**Settings → Secrets and variables → Actions**):

| Secret | Description | Example |
|--------|-------------|---------|
| `CORA_API_KEY` | Your LLM API key | `sk-...` |
| `CORA_BASE_URL` | LLM API base URL (optional) | `https://api.openai.com/v1` |
| `CORA_MODEL` | LLM model ID (optional) | `gpt-4o-mini` |

2. Create the workflow file:

```yaml
# .github/workflows/cora-review.yml
name: Cora AI Code Review

on:
  pull_request_target:
    branches: [main, develop]
    types: [opened, synchronize, ready_for_review, reopened]

concurrency:
  group: cora-review-${{ github.event.pull_request.number }}
  cancel-in-progress: true

permissions:
  contents: read
  pull-requests: write
  security-events: write

jobs:
  cora-review:
    name: Cora Review
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - name: Checkout PR head
        uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false

      - name: Run Cora AI Code Review
        uses: codecoradev/cora-review-action@v1
        with:
          github-token: ${{ secrets.GITHUB_TOKEN }}
          cora-api-key: ${{ secrets.CORA_API_KEY }}
          cora-base-url: ${{ secrets.CORA_BASE_URL }}
          cora-model: ${{ secrets.CORA_MODEL }}
```

### Anthropic with workload identity federation (no stored API key)

Instead of a long-lived `CORA_API_KEY`, the job can exchange its GitHub OIDC token for a short-lived Anthropic access token. Create a federation rule in the Anthropic Console (issuer `https://token.actions.githubusercontent.com`, audience `https://api.anthropic.com`, a **subject prefix** that matches your repository), then provide `ANTHROPIC_FEDERATION_RULE_ID`, `ANTHROPIC_ORGANIZATION_ID`, `ANTHROPIC_SERVICE_ACCOUNT_ID` and `ANTHROPIC_WORKSPACE_ID` (and optionally `ANTHROPIC_MODEL`) as organization or repository secrets or variables.

GitHub now issues subjects with numeric ids, for example `repo:my-org@289767358/my-repo@1254014703:pull_request`; a prefix written as `repo:my-org/my-repo:` never matches it. The Anthropic Console authentication log shows the reason for a denial (`match_subject_prefix`).

The job needs `permissions: id-token: write`. The exchanged token is sent only to `https://api.anthropic.com/v1` (never to `CORA_BASE_URL`), and when federation is unavailable the job falls back to the `CORA_*` secrets. This repository's `.github/workflows/cora-review.yml` and `anthropic-wif-smoke.yml` are a working reference.

### Options

| Input | Default | Description |
|-------|---------|-------------|
| `base-branch` | `origin/develop` | Base branch to compare against |
| `severity` | `major` | Minimum severity to report (`info`, `minor`, `major`, `critical`) |
| `cora-version` | `latest` | Pin a specific version (e.g., `v0.4.6`) |
| `upload-sarif` | `true` | Upload findings to GitHub Code Scanning |

### Custom Configuration

Add a `.cora.yaml` to your project root to customize review behavior:

```yaml
# .cora.yaml
focus:
  - security
  - bugs
  - performance

rules:
  - No unwrap() in production code
  - All public functions must have error handling

ignore:
  files:
    - "vendor/**"
    - "**/*.generated.*"
  rules:
    - "style"
```

### How It Works

The action automatically:

1. **Resolves** the latest cora-code version from GitHub releases
2. **Downloads** the binary with retry + checksum verification
3. **Runs** `cora review` on the PR diff
4. **Posts** a formatted comment on the PR with findings
5. **Uploads** SARIF results to GitHub Code Scanning (optional)
6. **Fails** the job if blocking issues (severity ≥ `major`) are found

### Minimal Setup (No Action)

If you prefer to run cora directly without the reusable action:

```yaml
# .github/workflows/cora-review.yml
name: Code Review

on:
  pull_request:
    branches: [main]

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - name: Install and run cora
        env:
          CORA_API_KEY: ${{ secrets.CORA_API_KEY }}
        run: |
          curl -fsSL https://raw.githubusercontent.com/codecoradev/cora-code/main/install.sh | sh
          cora review --base origin/main --format sarif
```

## 07 — Gitea / Forgejo CI

cora works on any CI platform that runs Linux. For Gitea and Forgejo, use the binary directly:

```yaml
# .gitea/workflows/cora-review.yml
name: Cora AI Code Review

on:
  pull_request:
    branches: [main, develop]

jobs:
  cora-review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Install cora-code
        run: |
          VERSION=$(curl -sf https://api.github.com/repos/codecoradev/cora-code/releases/latest | jq -r '.tag_name')
          curl -sfL "https://github.com/codecoradev/cora-code/releases/download/${VERSION}/cora-code-x86_64-unknown-linux-gnu.tar.gz" | tar xz
          sudo mv cora /usr/local/bin/

      - name: Run review
        env:
          CORA_API_KEY: ${{ secrets.CORA_API_KEY }}
          CORA_BASE_URL: ${{ secrets.CORA_BASE_URL }}
          CORA_MODEL: ${{ secrets.CORA_MODEL }}
        run: |
          cora review --base origin/${{ github.base_ref }} --format markdown > review.md

      - name: Post PR comment
        if: always()
        uses: actions/github-script@v7
        with:
          script: |
            const fs = require('fs');
            const body = fs.readFileSync('review.md', 'utf8');
            if (body.trim()) {
              await github.rest.issues.createComment({
                ...context.repo,
                issue_number: context.issue.number,
                body
              });
            }
```

### Gitea Secrets

Add these under **Settings → Actions → Secrets**:

| Secret | Required | Description |
|--------|----------|-------------|
| `CORA_API_KEY` | ✅ Yes | Your LLM API key |
| `CORA_BASE_URL` | No | LLM API base URL |
| `CORA_MODEL` | No | LLM model ID |

> **Note:** SARIF upload is GitHub-specific and not available on Gitea. Use `--format markdown` instead.

## 08 — GitLab CI

```yaml
# .gitlab-ci.yml
cora-review:
  stage: test
  image: rust:latest
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
  before_script:
    - VERSION=$(curl -sf https://api.github.com/repos/codecoradev/cora-code/releases/latest | jq -r '.tag_name')
    - curl -sfL "https://github.com/codecoradev/cora-code/releases/download/${VERSION}/cora-code-x86_64-unknown-linux-gnu.tar.gz" | tar xz
    - mv cora /usr/local/bin/
  script:
    - git fetch origin $CI_MERGE_REQUEST_TARGET_BRANCH_NAME
    - cora review --base origin/$CI_MERGE_REQUEST_TARGET_BRANCH_NAME --format markdown > review.md
  after_script:
    - |
      if [ -s review.md ]; then
        BODY=$(cat review.md)
        curl -sf -X POST \
          -H "PRIVATE-TOKEN: ${GITLAB_TOKEN}" \
          -H "Content-Type: application/json" \
          -d "{\"body\": $(echo "$BODY" | jq -Rs .)}" \
          "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/merge_requests/${CI_MERGE_REQUEST_IID}/notes"
      fi
  variables:
    CORA_API_KEY: $CORA_API_KEY  # Define in CI/CD Variables
```

### GitLab Variables

Add under **Settings → CI/CD → Variables**:

| Variable | Required | Description |
|----------|----------|-------------|
| `CORA_API_KEY` | ✅ Yes | Your LLM API key |
| `CORA_BASE_URL` | No | LLM API base URL |
| `CORA_MODEL` | No | LLM model ID |
| `GITLAB_TOKEN` | ✅ Yes | Personal access token with `api` scope (for posting MR comments) |

## 09 — Bitbucket Pipelines

```yaml
# bitbucket-pipelines.yml
pipelines:
  pull-requests:
    '**':
      - step:
          name: Cora AI Code Review
          image: rust:latest
          script:
            - apt-get update && apt-get install -y jq
            - |
              VERSION=$(curl -sf https://api.github.com/repos/codecoradev/cora-code/releases/latest | jq -r '.tag_name')
              curl -sfL "https://github.com/codecoradev/cora-code/releases/download/${VERSION}/cora-code-x86_64-unknown-linux-gnu.tar.gz" | tar xz
              mv cora /usr/local/bin/
            - git fetch origin $BITBUCKET_PR_DESTINATION_BRANCH
            - cora review --base origin/$BITBUCKET_PR_DESTINATION_BRANCH --format markdown > review.md
            - |
              if [ -s review.md ]; then
                BODY=$(cat review.md | jq -Rs .)
                curl -sf -X POST \
                  -H "Authorization: Bearer ${BB_AUTH_TOKEN}" \
                  -H "Content-Type: application/json" \
                  -d "{\"content\": {\"raw\": $BODY}}" \
                  "https://api.bitbucket.org/2.0/repositories/${BITBUCKET_WORKSPACE}/${BITBUCKET_REPO_SLUG}/pullrequests/${BITBUCKET_PR_ID}/comments"
              fi
```

### Bitbucket Variables

Add under **Repository settings → Pipelines → Repository variables**:

| Variable | Required | Description |
|----------|----------|-------------|
| `CORA_API_KEY` | ✅ Yes | Your LLM API key |
| `CORA_BASE_URL` | No | LLM API base URL |
| `CORA_MODEL` | No | LLM model ID |
| `BB_AUTH_TOKEN` | ✅ Yes | App password with `pullrequests:write` scope |

## 10 — Platform Comparison

| Feature | GitHub | Gitea / Forgejo | GitLab | Bitbucket |
|---------|--------|-----------------|--------|-----------|
| Marketplace action | ✅ Plug & play | ❌ | ❌ | ❌ |
| Binary install | ✅ | ✅ | ✅ | ✅ |
| Auto PR comment | ✅ Built-in | ✅ Via API | ✅ Via API | ✅ Via API |
| SARIF upload | ✅ Code Scanning | ❌ | ❌ | ❌ |
| Config file | `.cora.yaml` | `.cora.yaml` | `.cora.yaml` | `.cora.yaml` |
| Secrets setup | Repo secrets | Repo secrets | CI/CD variables | Repo variables |

> **Tip:** On all platforms, `cora review` works the same. The only difference is how you install the binary and post the comment back to the PR/MR.

## 11 — Pre-commit Hook

Install once, then every commit gets reviewed automatically.

```bash
# Install the hook
$ cora hook install

# Now just commit normally — cora reviews automatically
$ git commit -m "fix: handle edge case in parser"
cora pre-commit hook running...
No issues found — commit allowed
```

## 12 — SARIF Upload

Generate SARIF output and upload to GitHub Code Scanning.

```bash
# Generate SARIF report and upload
$ cora review --base main --format sarif > results.sarif && \
  cora upload-sarif results.sarif

Uploaded 3 findings to GitHub Code Scanning
```
