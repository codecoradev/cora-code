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

## Responsible Disclosure

We follow responsible disclosure principles:

1. Report privately first
2. Allow reasonable time to fix (typically 90 days, shorter for critical)
3. Coordinate public disclosure
4. Credit researchers (opt-in)
