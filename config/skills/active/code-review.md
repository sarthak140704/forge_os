---
name: code-review
version: 1.0.0
description: Review a diff or pull request for correctness, clarity, and risk.
tools:
  - fs.read
  - code.search
  - shell.run
triggers:
  keywords:
    - code review
    - diff
    - critique
    - feedback
    - reviewer
---
# Code Review Playbook

Use this playbook when the mission is to review changes rather than author
them. Aim for high signal: only raise issues that genuinely matter.

## Establish the diff
- `git diff <base>...<head>` (or `gh pr diff <n>`) to see exactly what changed.
- Read the surrounding code with `fs.read` — a hunk in isolation hides intent.

## What to look for (in priority order)
1. **Correctness** — logic errors, off-by-one, wrong operators, unhandled
   error paths, race conditions, missing `await`.
2. **Security & data safety** — injection, missing authz, secrets, destructive
   ops without guards.
3. **Regressions** — does this break an existing contract or test? Is there a
   test covering the new behaviour?
4. **Clarity** — names that mislead, dead code, duplicated logic that should be
   extracted.

## What NOT to comment on
- Pure formatting, import ordering, or style a linter/formatter already owns.
- Personal preference bikeshedding. Defer to the project's established
  conventions.

## Output
- Group findings by severity. For each: file:line, the concern, and a concrete
  suggestion. Distinguish blocking issues from nits explicitly. If the change
  is sound, say so plainly rather than inventing objections.
