# Forge OS

> Autonomous software-engineering runtime inspired by
> [Hermes Agent](https://hermes-agent.nousresearch.com/docs/), rebuilt as a
> persistent, event-sourced desktop OS with a mission-DAG execution model.

**Status:** Phase 1 vertical slice. See [`docs/ROADMAP.md`](docs/ROADMAP.md).

## What this proves

- Missions are first-class DAGs, not linear task lists.
- Everything is event-sourced (SQLite append-only log + in-process broadcast
  bus → live Tauri events → UI).
- LLM Router is provider-agnostic (OpenRouter, Ollama, Mock) with circuit
  breakers and failover.
- Every tool invocation passes through a declarative policy engine that can
  Allow / Deny / RequireApproval before the tool ever runs.
- Tauri v2 + React 18 + Tailwind + shadcn primitives + Zustand + TanStack Query
  + React Flow render the DAG live from streamed events.

## Prerequisites

### Windows

- **Rust 1.79+** (`rustup default stable-x86_64-pc-windows-msvc`)
- **Node.js 20+** (LTS recommended)
- **Visual Studio 2022 Build Tools** with the "Desktop development with C++"
  workload (Rust MSVC linker + Tauri bundling both need it)
- **Edge WebView2 Runtime** (usually preinstalled on Windows 11)

`winget install` names, in case any are missing:

```powershell
winget install Rustlang.Rustup
winget install OpenJS.NodeJS.LTS
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Microsoft.EdgeWebView2Runtime
```

### macOS

- **macOS 10.15+** (Catalina or newer)
- **Xcode Command Line Tools** — provides the linker and system SDK
- **Rust 1.79+**
- **Node.js 20+**
- WebKit is provided by the OS (WKWebView), no extra runtime needed

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
brew install node
# Optional: brew install ollama   # if you want a fully-local setup
```

Only shell-tool behavior branches per-OS (`sh -c` on macOS/Linux vs `cmd /C`
on Windows). Everything else — path handling, SQLite, LLM adapters,
event bus, UI — is portable.

**macOS icons for `tauri build`:** dev mode (`tauri dev`) uses the bundled
PNGs and just works. To produce a signed `.app`/`.dmg` you'll want a proper
`icon.icns`; generate one from `apps/forge-desktop/src-tauri/icons/icon.png`
with the macOS `iconutil` tool (see the Tauri docs).

### Linux

Same as macOS minus the Xcode step — you need `webkit2gtk` and the usual
build essentials. See the [Tauri prerequisites page](https://v2.tauri.app/start/prerequisites/)
for your distro.

## Configure an LLM provider

Forge tries providers in order; the first success wins. Set at least one:

```powershell
# Option A — OpenRouter (any model, unified billing)
$env:OPENROUTER_API_KEY = "sk-or-…"
$env:FORGE_MODEL        = "openai/gpt-4o-mini"

# Option B — Native OpenAI (sk-… key)
$env:OPENAI_API_KEY = "sk-…"
$env:OPENAI_ORG_ID  = "org-…"            # optional
$env:FORGE_MODEL    = "gpt-4o-mini"      # or gpt-4o, gpt-4.1, etc.

# Option C — Groq (very fast, generous free tier)
$env:GROQ_API_KEY = "gsk_…"
$env:FORGE_MODEL  = "llama-3.3-70b-versatile"   # or openai/gpt-oss-120b, moonshotai/kimi-k2-instruct, etc.

# Option D — local Ollama
ollama pull llama3.1
$env:FORGE_MODEL = "llama3.1"
```

If more than one is set, Forge fails over in the order
OpenRouter → OpenAI → Groq → Ollama. If none is available the runtime boots
but planning calls will fail; the UI will still render.

On macOS/Linux, replace `$env:FOO = "bar"` with `export FOO=bar` (bash/zsh).

## Run in dev mode

### Windows

```powershell
cd apps\forge-desktop\frontend
npm install
cd ..\src-tauri
..\frontend\node_modules\.bin\tauri.cmd dev
```

### macOS / Linux

```bash
cd apps/forge-desktop/frontend
npm install
cd ../src-tauri
../frontend/node_modules/.bin/tauri dev
```

The Tauri CLI spawns Vite for the frontend and Cargo for the backend, wiring
them together in a single desktop window.

## Run the workspace tests

```powershell
cd C:\path\to\forge-os
cargo test --workspace
```

## Where things live

```
forge-os/
├── crates/
│   ├── forge-domain/          pure types (leaf, no I/O)
│   ├── forge-events/          broadcast bus + append-only event store
│   ├── forge-persistence/     sqlx repositories (SQLite)
│   ├── forge-llm/             provider trait + OpenRouter/Ollama/Mock + Router
│   ├── forge-tools/           Tool trait + fs/shell/search built-ins
│   ├── forge-policy/          YAML rule engine → Allow/Deny/RequireApproval
│   ├── forge-planner/         LLM-backed mission → goal DAG
│   ├── forge-execution/       Tokio DAG walker
│   ├── forge-mission/         facade (create / plan / run / cancel)
│   └── forge-runtime/         composition root
├── apps/forge-desktop/
│   ├── src-tauri/             Tauri v2 backend + IPC commands
│   └── frontend/              Vite + React + Tailwind + React Flow
├── config/
│   ├── policy.default.yaml
│   └── skills/active/          seed Skills (rust-crate, node-project, python-project, git-repo)
└── docs/
    ├── IDEATION.md
    ├── ARCHITECTURE.md
    ├── ROADMAP.md
    └── SKILLS.md
```

## Phase 2 — Skills, Memory, Reflection, Continuous Re-planning

Phase 2 adds four capabilities on top of the Phase-1 core (no domain-model
changes, all wired through the existing event bus and repositories):

**Skills** — versioned Markdown playbooks under `{skills_root}/active/`.
On mission start, the `SkillRegistry` scores every active skill by keyword
match against the mission title + description; the top 4 are injected into
the planner's system prompt. See [`docs/SKILLS.md`](docs/SKILLS.md) for the
`SKILL.md` format and author guide. Seed skills ship in
`config/skills/active/` and are copied to `%APPDATA%\com.sarthak.forgeos\skills`
on first launch.

**Project memory** — Forge reads the *workspace* root for `.forge.md` >
`AGENTS.md` > `CONTRIBUTING.md` (first hit wins, capped at 8 KB) and
injects it into every planner call as a "Project conventions" section. Use
this for per-repo rules that shouldn't be reusable across projects.

**Continuous re-planning** — after every execution wave, `MissionService`
asks the planner for a `PlanDelta`. New goals/tasks are persisted, existing
ids are preserved, and execution resumes. Capped at 5 replan passes and 30
total goals per mission to keep runs bounded. Emits `ReplanRequested`,
`PlanRevised`, and `ReplanCapExceeded` events.

**Reflection → Skill proposals** — after a mission reaches terminal state,
the `Reflector` asks the LLM for a post-mortem (`what_worked`,
`what_failed`, `insights`, `suggested_skills`). The reflection is persisted
to a `reflections` table; each suggested skill is written to
`{skills_root}/proposed/` with `status: pending_review`. **Proposals are
never auto-activated** — approve or reject via the
`approve_skill_proposal` / `reject_skill_proposal` IPC commands. Emits
`MissionReflectionCompleted` and one `SkillProposalWritten` per suggestion.

## MCP plugins

Forge speaks the [Model Context Protocol](https://modelcontextprotocol.io/) as
a client. Uncomment an entry in `mcp.yaml` (auto-seeded on first boot) to spawn
an MCP server on startup; its tools appear as `mcp.<server>.<tool>` alongside
the built-ins. Failures never block boot — they emit `McpServerFailed` and the
runtime carries on. Full docs: [`docs/MCP.md`](docs/MCP.md).

## What's next

Phase 2+ items — semantic memory, OpenTelemetry, provider expansion,
sandboxed plugins, macOS/Linux builds — are enumerated in
[`docs/ROADMAP.md`](docs/ROADMAP.md). None of them require changes to the
domain model or the event bus; they attach through the same seams the Phase-1
subsystems use.

Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>
