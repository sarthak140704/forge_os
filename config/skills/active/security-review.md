---
name: security-review
version: 1.0.0
description: Audit a codebase for secrets, vulnerable dependencies, and unsafe patterns.
tools:
  - fs.read
  - fs.list
  - code.search
  - shell.run
triggers:
  keywords:
    - security
    - vulnerability
    - audit
    - secret
    - cve
    - hardening
---
# Security Review Playbook

Use this playbook when the mission is to assess or harden security. This is a
**read-and-report** skill by default — propose fixes, do not silently rewrite
security-sensitive code without approval.

## Secret scanning
- `code.search` for high-signal patterns: `api_key`, `secret`, `password`,
  `BEGIN PRIVATE KEY`, `AKIA` (AWS), `xox` (Slack), `ghp_`/`github_pat_`.
- Check that `.env`, `*.pem`, and credential files are `.gitignore`d and not
  committed (`git ls-files | grep -Ei 'env|pem|key'`).

## Dependency audit
- Node: `npm audit --production`. Rust: `cargo audit`. Python: `pip-audit`.
- Report each finding with severity, the affected version, and the fixed
  version — do not auto-bump majors without checking for breaking changes.

## Code patterns to flag
- Injection: string-built SQL/shell commands using untrusted input.
- Missing authn/authz checks on mutating endpoints.
- Broad CORS (`*`), disabled TLS verification, weak crypto (MD5/SHA1 for
  passwords instead of bcrypt/argon2).
- Overly permissive file/permission modes and least-privilege violations.

## Output
- Produce a prioritised findings list (Critical → Low) with file:line and a
  concrete remediation for each. Only apply low-risk, well-understood fixes
  automatically; route the rest through an approval.
