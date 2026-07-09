# Skills — authoring guide

**Skills** are versioned Markdown playbooks that Forge injects into planner
context when their triggers match a mission. Think of them as reusable
"how-to" cards: *how to work with a Rust crate*, *how to bootstrap a Node
project*, *how to run a Python test suite*.

Skills are **not** code. They are prompt fragments. The planner is free to
ignore them, but by seeding it with strong domain conventions we get better
plans on the first try.

## File layout

```
{skills_root}/
  active/         ← loaded on boot
    rust-crate.md
    node-project.md
  proposed/       ← written by the reflection pass; require approval
    my-new-skill-2026-01-01T00-00-00Z.md
  archived/       ← never loaded (soft-delete)
```

At runtime the desktop app resolves `{skills_root}` to
`%APPDATA%\com.sarthak.forgeos\skills` (or the equivalent per-OS path). On
first launch the bundled seed skills are copied in automatically.

### Seed catalogue (20 skills)

The bundled `active/` set (see `config/skills/active/` and `SEED_SKILLS` in the
desktop bootstrap) covers common software-engineering domains:

| Domain | Skills |
|--------|--------|
| Languages / build | `rust-crate`, `go-module`, `python-project`, `node-project`, `react-app` |
| Source control | `git-repo`, `github-cli` |
| Containers / infra | `docker`, `kubernetes`, `terraform`, `aws` |
| Data stores | `postgres`, `redis`, `database-migration` |
| Practices | `code-review`, `security-review`, `refactoring`, `documentation`, `release-management`, `incident-response` |

Every seed skill is gated by `crates/forge-skills/tests/seed_skills.rs`, which
asserts each one passes the hard validation checks (parses, body length, has a
trigger, declares resolvable tools) against the built-in tool set.

## `SKILL.md` format

Every skill is a Markdown file with YAML front-matter:

```markdown
---
name: rust-crate
version: 0.1.0
status: active               # active | pending_review | archived
description: |
  Playbook for working inside a Rust crate: format, lint, test.
triggers:
  keywords: [rust, cargo, crate, clippy, rustc]
  file_globs: ["**/Cargo.toml"]
tools: [shell.run, fs.read, fs.write]
inputs:  ["workspace_root"]
outputs: ["build_log", "test_log"]
---

# Rust crate playbook

1. Run `cargo fmt --all` before making any style-sensitive edits.
2. `cargo clippy --workspace --all-targets -- -D warnings` must pass.
3. `cargo test -p <crate>` for a fast per-crate check; full suite only on
   validation.
...
```

### Required fields

- `name` — kebab-case, unique across skills. Used for dedup.
- `version` — semver-ish (`MAJOR.MINOR.PATCH`). Highest version wins if two
  files declare the same name.
- `description` — one paragraph shown to the planner.

### Optional fields

- `status` — defaults to `active`. `pending_review` skills live in
  `proposed/` and are never loaded until approved.
- `triggers.keywords` — matched against mission title + description with a
  word-boundary check (so `rust` won't match `trust`). Two occurrences in
  the title score higher than one in the description.
- `triggers.file_globs` — currently informational; will be used by a future
  workspace-aware selector.
- `tools`, `inputs`, `outputs` — informational; help future tool-use models
  reason about pre-requisites.

The Markdown body is passed to the planner verbatim.

## How skills are selected

`SkillRegistry::select_for_mission(title, desc)` scores every active skill
by keyword hits (title = 2×, description = 1×) and returns the top matches.
`MissionService` takes the top **4** and injects them into the planner's
system prompt as a "Skills available" section.

## Reflection → proposals

At the end of every mission that had an LLM available, the `Reflector` asks
the model to post-mortem the run and (optionally) suggest new skills. Each
suggestion is written to `{skills_root}/proposed/{name}-{timestamp}.md`
with `status: pending_review`. **They are never auto-activated.**

Approve or reject via IPC (or the desktop UI once it lands):

```rust
forge_skills::proposal::approve_proposal(&skills_root, "my-skill-....md")?;
forge_skills::proposal::reject_proposal(&skills_root, "my-skill-....md")?;
```

Approving moves the file to `active/` and flips its front-matter status to
`active`. Rejecting just deletes it.

## Project memory

A separate mechanism: Forge also looks at the *workspace* root for a
project-specific memory file, in this precedence:

1. `.forge.md`
2. `AGENTS.md`
3. `CONTRIBUTING.md`

The first one found (capped at 8 KB) is injected into every planner call
under a "Project conventions" section. Use this for repo-local rules that
aren't reusable across projects (branch naming, commit style, "always run
X before Y", etc.).
