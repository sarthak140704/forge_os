---
name: go-module
version: 1.0.0
description: Build, test, vet, and tidy a Go module or multi-package workspace.
tools:
  - fs.read
  - fs.write
  - shell.run
triggers:
  keywords:
    - go
    - golang
    - gofmt
    - govet
    - module
  file_globs:
    - "**/go.mod"
    - "**/go.sum"
    - "**/*.go"
---
# Go Module Playbook

Use this playbook whenever the mission involves a Go module.

## Preflight
1. Read `go.mod` to confirm the module path and Go version. Respect the
   declared `go` directive — do not bump it casually.
2. `go env GOFLAGS GOWORK` — note whether a `go.work` file scopes the build.

## Edit loop
- After each change, `go build ./...` for the affected packages.
- `gofmt -l .` to find unformatted files; fix with `gofmt -w <file>`.
- `go vet ./...` catches common correctness bugs the compiler misses.

## Validation
- `go test ./... -race -count=1` for the changed packages; the `-race` flag is
  cheap insurance for concurrent code and `-count=1` defeats the test cache.
- `go mod tidy` after adding/removing imports, then confirm `go.sum` changes
  are intentional before committing.

## Dependencies
- Add with `go get <module>@<version>`; prefer tagged releases over pseudo
  versions. Never edit `go.sum` by hand.

## Rollback
- Revert `go.mod`/`go.sum` with `git checkout -- go.mod go.sum` and re-run
  `go mod tidy` to restore a consistent dependency graph.
