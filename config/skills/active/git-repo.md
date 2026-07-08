---
name: git-repo
version: 1.0.0
description: Inspect, edit, commit, and push changes to a Git repository safely.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - git
    - commit
    - branch
    - merge
    - pull request
    - pr
    - github
    - gitlab
---
# Git Repository Playbook

Use this playbook whenever the mission modifies a Git-tracked codebase.

## Read before writing
- `git status --short` to know what's already dirty.
- `git log --oneline -20` to understand recent history and the current
  branch's cadence.
- `git branch --show-current` — never assume you're on `main`.

## Branching
- For any non-trivial change, create a working branch:
  `git checkout -b forge/<mission-slug>`.
- Do **not** work directly on `main` / `master` / `trunk` — those are
  protected by convention.

## Committing
- Stage narrowly: `git add <specific files>`. Avoid `git add .` unless the
  mission explicitly says "commit everything".
- Commit messages: imperative mood, ≤72 chars in the summary line, body
  explains **why** (not what — the diff shows the what).
- Never `git commit --amend` on a commit that has already been pushed to a
  shared branch.

## Pushing
- `git push -u origin <branch>` on the first push of a branch.
- Never `git push --force` on a shared branch. `--force-with-lease` is the
  minimum acceptable safety net if a rewrite is genuinely required.

## History rewriting
- Never rebase or squash a branch that other people are actively working on
  without coordinating first.

## Rollback
- Uncommitted changes: `git checkout -- <file>` or `git restore <file>`.
- Bad commit not yet pushed: `git reset --soft HEAD~1` (keeps changes),
  `git reset --hard HEAD~1` (discards).
- Bad commit already pushed: `git revert <sha>` — never `reset --hard` on
  shared history.
