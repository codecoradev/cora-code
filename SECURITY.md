# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.13.x  | :white_check_mark: |
| 0.12.x  | :white_check_mark: |
| < 0.12  | :x:                |

## Reporting a Vulnerability

Cora processes source code and diffs — security vulnerabilities could expose user code, secrets in config files, or the LLM API keys used for review.

**Do NOT open a public issue for security vulnerabilities.**

Instead, email **hello@codecora.dev** with subject `[Cora security]`. Include:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested fix (optional)

You can expect:

- **Acknowledgment within 48 hours**
- **Assessment within 7 days**
- **Fix or workaround communicated to you before public disclosure**

If the vulnerability is accepted, we will coordinate disclosure timing with you. Credit is given unless you prefer anonymity.

## Security Considerations in Cora

Cora handles security-sensitive data:

- **API keys** — LLM provider keys stored in config files and environment variables
- **Source code** — Full access to indexed project files
- **Git history** — Reads diffs, blame data, and commit metadata
- **Pre-commit hooks** — Runs automatically on `git commit`, processes staged content

Security-related areas of the codebase:

- `src/engine/rules/` — Secret detection patterns
- `src/config/providers.rs` — API key handling
- `src/hook/` — Pre-commit hook integration
- `src/index/` — File access and SQLite storage

## Threat Model: Adversarial Source-Code Comments (ALIBI)

LLM-based reviewers are vulnerable to adversarial comments in the code under
review that steer reviewer reasoning without changing program behavior —
attack success exceeds 90% across 125 real-world vulnerabilities, with
fabricated tool-result claims ("sanitizer passed", "already validated") being
the most effective vector (arXiv:2607.24964).

**Prompt-level defenses (telling the model to ignore comments) are proven
ineffective against adaptive attacks.** Cora therefore uses architectural
defenses:

- **Claim flagging (always on)** — added comments asserting verification or
  tool results are detected heuristically and injected into review context as
  *untrusted claims*, never as facts.
- **Comment sanitization (opt-in)** — set `review.sanitize-comments: true` in
  `.cora.yaml` to strip comment bodies from added diff lines before the LLM
  sees them. Line structure is preserved (`[comment removed]` markers), so
  findings still map to real line numbers. Deterministic scanners (rules,
  secrets, security patterns) always run on the *unsanitized* diff.
- Sanitization is heuristic (line-comment markers `//`, leading `#`, `--`,
  `;`); block comments (`/* */`, `"""..."""`) are not currently stripped.

Relevant code: `src/engine/comment_sanitizer.rs`.

## Responsible Disclosure

We follow responsible disclosure principles:

1. Report privately first
2. Allow reasonable time to fix (typically 90 days, shorter for critical)
3. Coordinate public disclosure
4. Credit researchers (opt-in)
