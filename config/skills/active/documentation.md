---
name: documentation
version: 1.0.0
description: Write and maintain READMEs, API docs, and runbooks grounded in the code.
tools:
  - fs.read
  - fs.write
  - fs.list
  - code.search
triggers:
  keywords:
    - documentation
    - readme
    - docs
    - runbook
    - changelog
    - guide
  file_globs:
    - "**/README*.md"
    - "**/docs/**/*.md"
    - "**/CHANGELOG*.md"
---
# Documentation Playbook

Use this playbook when the mission is to write or update documentation.

## Ground the docs in reality
- Read the actual code, config, and scripts before writing. Never document a
  command or flag you have not seen in the source — verify with `code.search`.
- Prefer runnable, copy-pasteable examples over prose. Show the exact command
  and the expected output.

## Structure a README
1. One-sentence "what it is" + the problem it solves.
2. Quick start: install → configure → run, in ≤5 commands.
3. Configuration reference (env vars / flags) as a table.
4. Architecture overview only if it helps a contributor orient.
5. Troubleshooting for the failure modes users actually hit.

## Runbooks
- Write for the on-call engineer at 3am: numbered, imperative steps; the exact
  commands; how to confirm success; how to roll back.

## Maintenance
- When code changes behaviour, update the docs in the same mission — stale docs
  are worse than none. Keep a `CHANGELOG.md` in Keep-a-Changelog format.

## Validation
- Re-read the doc as a new user and follow every command literally. Fix
  anything that does not work exactly as written.
