---
name: rust-crate
version: 1.0.0
description: Format, lint, test, and validate a Rust crate or Cargo workspace.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - rust
    - cargo
    - crate
    - clippy
    - rustc
  file_globs:
    - "**/Cargo.toml"
    - "**/*.rs"
---
# Rust Crate Playbook

Use this playbook whenever the mission involves a Rust crate or Cargo workspace.

## Scaffolding a new crate
When creating a new crate from scratch, **skip explicit directory creation** —
`fs.write` auto-creates parent directories. So writing `mycrate/Cargo.toml`
and `mycrate/src/main.rs` is enough; the `mycrate/` and `mycrate/src/` folders
appear automatically.

Only use `fs.mkdir` for empty directories that will have no files (rare).

## Preflight
1. Read `Cargo.toml` (or the workspace `Cargo.toml`) to confirm the crate name,
   edition, and any workspace members that scope the work.
2. If `rust-toolchain.toml` exists, note the pinned toolchain — do not override.

## Edit loop
- After every code change, run `cargo check -p <crate>` in the affected crate.
  If the change touched shared types, run `cargo check --workspace`.
- Prefer `cargo clippy -p <crate> --all-targets -- -D warnings` before
  declaring the change done. Treat warnings as errors.
- Use `cargo fmt -p <crate>` (never `--check` in the middle of editing — run
  it as a fix, then run `--check` at the end to verify).

## Validation
- `cargo test -p <crate> --all-targets` for the changed crate.
- `cargo test --workspace --exclude forge-desktop` when touching cross-crate
  APIs (the desktop crate needs Tauri build tools and is not a portable test).
- If a test fails, read the test source and the failing assertion **before**
  editing production code — the test may encode intent you did not know about.

## Rollback
- Every change should be reversible via `git checkout -- <file>` or by
  restoring the prior version from the shadow workspace.
- Never `cargo clean` on a shared workspace — it will invalidate other
  concurrent missions' cache.
