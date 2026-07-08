# MCP (Model Context Protocol) in Forge OS

> **From the spec:** *"Prefer MCP whenever available. Allow native implementations when necessary."*

Forge OS speaks MCP as a **client**: it spawns MCP servers as child processes,
handshakes over stdio, and exposes each server's tools to the planner
alongside the built-in `fs.*`, `code.*`, and `shell.run` tools.

One YAML entry unlocks an entire tool ecosystem — filesystem, git, GitHub,
Playwright, brave-search, postgres, sqlite, and anything else the community
publishes.

## Configuration

Edit `mcp.yaml` (auto-seeded on first boot at
`%APPDATA%\com.sarthak.forgeos\mcp.yaml` on Windows, or
`~/Library/Application Support/com.sarthak.forgeos/mcp.yaml` on macOS).

Minimum viable entry:

```yaml
servers:
  - name: filesystem
    command: npx
    args: ["-y", "@modelcontextprotocol/server-filesystem", "${workspace_root}"]
```

Full schema:

| Field       | Required | Default | Notes                                                                 |
|-------------|:--------:|---------|-----------------------------------------------------------------------|
| `name`      | ✅       | —       | Logical name used to namespace tools: `mcp.<name>.<tool>`.            |
| `command`   | ✅       | —       | Executable to spawn. Looked up on `PATH`.                             |
| `args`      |          | `[]`    | Command-line args. `${workspace_root}` is expanded per invocation.    |
| `env`       |          | `{}`    | Env additions (parent env is inherited).                              |
| `cwd`       |          | inherit | Working dir for the child.                                            |
| `transport` |          | `stdio` | Only `stdio` today.                                                   |
| `enabled`   |          | `true`  | Set `false` to keep a definition but skip spawn.                      |

## How it fits together

```
config/mcp.yaml
      │
      ▼
McpRegistry::start ─┬─▶ StdioTransport::spawn (child, kill_on_drop)
                    │
                    │  JSON-RPC 2.0 (line-delimited)
                    │
                    ├─▶ initialize / notifications/initialized  (handshake)
                    ├─▶ tools/list                                (metadata)
                    │
                    ▼
For each tool:
  McpToolProxy → registers as `mcp.<server>.<tool>` in the ToolRegistry.
  invoke() forwards to tools/call and flattens the content parts.
```

* **Namespacing** — tool names become `mcp.<server>.<tool>`, e.g.
  `mcp.filesystem.read_file`. That guarantees no collision with built-ins
  and makes it obvious in the event log where a call went.
* **Failure isolation** — a failed handshake or spawn emits
  `McpServerFailed` and the runtime carries on. MCP never blocks boot.
* **Lifetime** — dropping the `Runtime` drops the `McpRegistry` drops each
  client drops its transport drops its `ChildGuard` which SIGKILLs the
  child. No orphans.

## Events

Every MCP interaction produces domain events (queryable via `SELECT ...
FROM events` in the sqlite store, and broadcast in-process to the Tauri UI):

* `mcp_server_started { name, tools[] }` — one per successfully-connected server.
* `mcp_server_failed { name, error }`   — one per failed server.
* `mcp_tool_invoked { server, tool, task_id }` — one per call boundary.

## Vetted servers to try

| Server                                          | Install                                                                              | Notes                                                     |
|-------------------------------------------------|--------------------------------------------------------------------------------------|-----------------------------------------------------------|
| `@modelcontextprotocol/server-filesystem`       | `npx -y @modelcontextprotocol/server-filesystem <root>`                              | Sandboxes reads/writes to `<root>`. Great starting point. |
| `mcp-server-git`                                | `pip install mcp-server-git`                                                         | Real git plumbing without shelling out.                   |
| `@modelcontextprotocol/server-github`           | `npx -y @modelcontextprotocol/server-github` + `GITHUB_PERSONAL_ACCESS_TOKEN`         | Issues, PRs, code search.                                 |
| `@modelcontextprotocol/server-brave-search`     | `npx -y @modelcontextprotocol/server-brave-search` + `BRAVE_API_KEY`                 | Web search.                                               |
| `@modelcontextprotocol/server-sqlite`           | `pipx install mcp-server-sqlite` (or npx)                                            | Query local dbs.                                          |

See <https://github.com/modelcontextprotocol/servers> for the full list.

## Security notes

* Every MCP tool ships with the union of `FsRead | FsWrite | Network | Shell`
  permissions in its schema. The **PolicyEngine** is the enforcement point —
  add per-server rules there if you need to restrict a specific server's
  reach (e.g. deny `mcp.github.*` unless a mission is tagged
  `github-allowed`).
* MCP servers run as full OS processes with your user's permissions. Only
  enable servers whose provenance you trust. Sandboxing (Wasmtime) is a
  Phase-3 item.

## Writing your own MCP server

Follow the MCP spec at <https://modelcontextprotocol.io/specification>. The
easiest path is the TypeScript or Python SDK — implement `tools/list` and
`tools/call`, publish to npm/pip, add a `servers:` entry, done.

## Debugging

* Set `RUST_LOG=forge_mcp=debug` — the transport writes stderr from each
  child at `debug` level under the child's command name, so you'll see the
  server's own logs interleaved with Forge's.
* Look for `mcp_server_started` / `mcp_server_failed` in the event log:
  `SELECT event_type, payload FROM events WHERE aggregate_kind='plugin'`.
