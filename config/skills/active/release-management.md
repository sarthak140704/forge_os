---
name: release-management
version: 1.0.0
description: Cut a versioned release with changelog, tag, and verified artifacts.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - release
    - versioning
    - semver
    - tag
    - publish
    - changelog
---
# Release Management Playbook

Use this playbook when the mission is to cut a release. For the raw `gh release`
mechanics, compose with the `github-cli` skill.

## Preflight — is main releasable?
1. CI on the release commit must be fully green. Never release on red or
   pending checks.
2. Working tree clean; you are on the release branch/`main` at the intended
   commit (`git log --oneline -5`).

## Choose the version (SemVer)
- **patch** for backwards-compatible bug fixes, **minor** for backwards-
  compatible features, **major** for breaking changes. When unsure whether a
  change is breaking, treat it as breaking.

## Changelog
- Update `CHANGELOG.md` (Keep-a-Changelog): move items from *Unreleased* into a
  new `## [x.y.z] - <date>` section grouped by Added/Changed/Fixed/Removed.
- Bump the version in the manifest(s) (`package.json`, `Cargo.toml`,
  `pyproject.toml`) — keep them in sync.

## Tag and publish
- Annotated tag: `git tag -a v<x.y.z> -m "Release v<x.y.z>"` then
  `git push origin v<x.y.z>`.
- Build the release artifacts and **verify** them (checksums, a smoke run)
  before publishing. Attach artifacts to the release.

## Rollback
- A bad release is fixed by publishing a higher patch version, not by deleting
  the tag/release others may have pulled. Only delete a tag if it was pushed by
  mistake and no one has consumed it.
