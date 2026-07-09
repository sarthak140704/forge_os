---
name: react-app
version: 1.0.0
description: Build, lint, type-check, and test a React + TypeScript frontend.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - react
    - jsx
    - tsx
    - component
    - frontend
    - vite
  file_globs:
    - "**/*.tsx"
    - "**/*.jsx"
    - "**/vite.config.*"
    - "**/tailwind.config.*"
---
# React App Playbook

Use this playbook for React + TypeScript frontends (Vite / Next / CRA).

## Preflight
1. Read `package.json` scripts — use the project's own `dev`/`build`/`lint`/
   `test` scripts rather than inventing commands.
2. Note the toolchain: Vite vs Next, the test runner (Vitest/Jest), and whether
   TypeScript `strict` is on.

## Edit loop
- Type-check continuously: `tsc --noEmit` (or `npm run typecheck`). Treat type
  errors as build failures.
- Lint with the project config: `npm run lint`. Fix, don't suppress, unless a
  rule is genuinely wrong for the file.

## Component conventions
- Keep components small and typed; derive prop types explicitly. Prefer
  composition over prop-drilling; lift shared state into a store (Zustand) or
  context only when two siblings need it.
- Co-locate styles (Tailwind classes / CSS modules) with the component.

## Validation
- `npm run build` must pass (this runs `tsc` + the bundler in most setups).
- `npm test` (Vitest/Jest) for the changed components; add a test when you fix
  a bug so it can't regress.

## Rollback
- Frontend changes are pure source: `git checkout -- <files>` reverts cleanly.
  Re-run `npm run build` to confirm the tree is green after reverting.
