---
name: github-cli
version: 1.0.0
description: Manage pull requests, issues, releases, and Actions via the gh CLI.
tools:
  - fs.read
  - shell.run
triggers:
  keywords:
    - gh
    - github cli
    - issue
    - release
    - workflow
    - actions
    - review
  file_globs:
    - "**/.github/workflows/*.yml"
    - "**/.github/workflows/*.yaml"
---
# GitHub CLI Playbook

Use this playbook for GitHub *platform* work (PRs, issues, releases, CI). For
raw local git operations use the `git-repo` skill instead.

## Preflight
1. `gh auth status` — confirm an authenticated token with the needed scopes.
2. `gh repo view --json nameWithOwner,defaultBranchRef` to anchor the repo.

## Pull requests
- Open: `gh pr create --fill --base <default> --head <branch>` (add
  `--draft` when work is incomplete).
- Review state: `gh pr status` and `gh pr checks` — never merge with red CI.
- Merge respecting protections: `gh pr merge --squash --auto` so the merge
  waits for required checks rather than bypassing them.

## Issues
- `gh issue list --state open --limit 30` to triage.
- `gh issue create --title ... --body ...`; link PRs with `Closes #<n>` in the
  PR body so the issue auto-closes on merge.

## Releases
- Tag then release: `gh release create v<x.y.z> --generate-notes`. Attach
  build artifacts with `--attach <file>`.

## Actions
- `gh run list --workflow <name>` and `gh run view <id> --log-failed` to
  diagnose failing pipelines before re-running with `gh run rerun <id>`.

## Safety
- Never force-merge past branch protection or delete releases/tags that others
  may depend on without an explicit approval.
