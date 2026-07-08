---
name: node-project
version: 1.0.0
description: Bootstrap, build, and test a Node.js / npm / pnpm / yarn project.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - node
    - npm
    - pnpm
    - yarn
    - typescript
    - javascript
    - vite
    - react
    - nextjs
  file_globs:
    - "**/package.json"
    - "**/*.ts"
    - "**/*.tsx"
---
# Node Project Playbook

Use this playbook whenever the mission touches a Node.js codebase.

## Detect the package manager (in this order)
1. `pnpm-lock.yaml` → use `pnpm`.
2. `yarn.lock`      → use `yarn`.
3. `package-lock.json` or none of the above → use `npm`.

Never mix package managers — the lockfile is authoritative.

## Preflight
- Read `package.json` for `scripts`, `engines.node`, and top-level deps.
- Read `tsconfig.json` if it exists to confirm strict mode + module resolution.
- If `.nvmrc` or `engines.node` pins a version, respect it — do not upgrade.

## Install
- First run: `npm install` (or the detected equivalent). Subsequent runs may
  skip install if `node_modules/` is already present unless dependencies
  changed.

## Edit loop
- After code changes, run the project's own scripts in preference to guessing:
  - `npm run lint` if `scripts.lint` exists
  - `npm run typecheck` or `npx tsc --noEmit` if TypeScript
  - `npm run build` before declaring a feature done
- Prefer `npm run test -- --run` (or `vitest run` / `jest --ci`) so the
  runner exits after one pass instead of watching.

## Validation
- If the project has a CI script (e.g. `npm run ci`), run that as the final
  gate — it captures the team's intent.
- Never commit `node_modules/` or `.env`.

## Rollback
- `git checkout -- package.json package-lock.json` if a dependency change
  needs to be undone. Delete `node_modules/` and reinstall.
