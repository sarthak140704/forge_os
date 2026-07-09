---
name: refactoring
version: 1.0.0
description: Restructure code safely behind a green test suite without changing behaviour.
tools:
  - fs.read
  - fs.write
  - code.search
  - shell.run
triggers:
  keywords:
    - refactor
    - cleanup
    - restructure
    - rename
    - extract
    - technical debt
---
# Refactoring Playbook

Use this playbook when the mission improves structure **without** changing
observable behaviour.

## Golden rule
- A refactor is only safe behind a passing test suite. If coverage is thin for
  the area you are changing, **add characterisation tests first** that pin the
  current behaviour, then refactor.

## Preflight
1. Run the full relevant test suite and confirm it is green *before* touching
   anything. Record the baseline.
2. `code.search` for every call site of the symbol you are about to change —
   renames and signature changes ripple.

## Small, reversible steps
- Make one mechanical change at a time (extract function, rename, inline,
  move). Re-run tests after each step, not just at the end.
- Prefer the language's refactoring tooling (IDE rename, `cargo fix`,
  `gofmt -r`) over hand-editing many files.

## Guardrails
- Do not mix a refactor with a behaviour change in the same commit — reviewers
  cannot tell which diff line is which. Separate them.
- Keep public APIs stable unless the mission explicitly allows breaking them.

## Validation & rollback
- The suite must be exactly as green after as before; the diff should change
  structure, not test outcomes. If a test's *expected value* had to change,
  that was a behaviour change — stop and reconsider.
- Every step is a separate commit so any one can be reverted independently.
