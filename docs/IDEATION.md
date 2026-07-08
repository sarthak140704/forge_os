# Forge OS — Ideation

Source spec: `C:\Users\t-sarverma\Downloads\agent.txt`
Reference: https://hermes-agent.nousresearch.com/docs/ (see `llms.txt` index)

## 1. Framing

The spec asks for a persistent, autonomous SWE OS ("Forge OS") — not a chatbot, not
an IDE extension. Positioning is enterprise-grade (Microsoft/OpenAI/Anthropic/Stripe
class). It calls out 35 deliverables spanning domain model, engines, plugin/tool SDKs,
security, observability, DDD/CQRS/event-sourcing.

Realistically that is a **multi-year, multi-team platform**. For a personal project the
right move is to build the *backbone* that makes the rest additive, and ship a real
end-to-end vertical slice that proves the architecture rather than skeleton-scaffolding
all 30+ subsystems shallowly.

## 2. What is genuinely differentiated vs. Hermes Agent

Hermes is a mature reference implementation of *terminal-native autonomous agent*
with skills, curator, delegation, kanban, cron, MCP, provider routing, checkpoints,
21+ messaging platforms. Where Forge OS diverges meaningfully:

| Forge OS primitive              | Novel vs. Hermes?                                                                 |
|---------------------------------|-----------------------------------------------------------------------------------|
| Mission as a first-class DAG    | Yes — Hermes' Kanban is task list + Ralph loop; Forge treats work as a graph.     |
| Event-sourced runtime           | Yes — Hermes has session storage, no formal ES.                                   |
| Declarative Policy Engine       | Yes — Hermes has approval prompts, not YAML-configurable policies.                |
| Persistent runtime > months     | Partial — Hermes has cron + Modal/Daytona persistence; Forge makes it primary.    |
| Skill versioning + promotion    | Yes — Hermes Curator archives; Forge versions and gates on validation.            |
| LLM Router with cost/latency SLO| No — Hermes already has provider-routing/fallback/credential-pools. **Reuse.**    |
| Plugin sandbox + capability sys | Yes — Hermes plugins run in-process; Forge requires capability-based isolation.   |

## 3. Steal from Hermes (design patterns, not code)

- **SKILL.md** format — adopt for interop with `agentskills.io` and community skills
- **MCP-first, native-fallback** plugins — matches your spec exactly
- **Hooks on lifecycle** — subscribers on your Event Bus
- **Delegate/subagent** — becomes sub-mission spawning with its own goal DAG
- **Shadow-git checkpoints** — filesystem rollback for the Execution Engine
- **Context files (AGENTS.md, .hermes.md, SOUL.md)** — Project + User Memory layer
- **Fallback providers, credential pools** — LLM Router design

## 4. Reality-checked Phase plan

### Phase 1 (this session) — Vertical Slice
The minimum that proves *event-sourced mission-DAG runtime with pluggable LLM/tools
and declarative policy* on a Tauri v2 + React UI. Real code, `cargo check` green,
`vite build` green, `tauri dev` boots and shows a live mission running against a real
LLM endpoint (OpenRouter or Ollama).

- `forge-domain` — Mission / Goal / Task / typed IDs / event enum / state machines
- `forge-events` — Tokio broadcast bus + SQLite append-only event store (event sourcing)
- `forge-persistence` — sqlx + SQLite + migrations + repository trait
- `forge-llm` — `LlmProvider` trait + OpenRouter + Ollama adapters + Router w/ fallback
- `forge-tools` — `Tool` trait + `read_file` / `write_file` / `run_command` / `search_code` / `list_dir`
- `forge-policy` — YAML rule loader → `Allow | RequireApproval | Deny` + audit trail
- `forge-planner` — LLM-backed decomposer (JSON-schema constrained) producing a goal DAG
- `forge-execution` — Tokio DAG walker: dependency resolution, retries, checkpoints, cancellation, events
- `forge-mission` — facade (create / pause / resume / cancel)
- `forge-runtime` — composition root, exposed to Tauri IPC
- `apps/forge-desktop` — Tauri v2 + Vite/React/TS/Tailwind/shadcn/Zustand/TanStack Query/React Flow/Monaco/xterm.js
  - Views: Mission list, Mission DAG (React Flow), Event timeline (live stream), Tool execution log

### Phase 2 — Extensibility
- Skill Runtime v2 (loads `SKILL.md`, executes procedural graphs)
- MCP client (host MCP servers as plugins)
- Working Memory / Project Memory / Semantic Memory (embedded RAG via sqlite-vss)
- OpenTelemetry (tracing, metrics)
- Cross-platform (macOS + Linux builds)

### Phase 3 — Governance & Safety
- Plugin sandboxing (Wasmtime or child-process capabilities)
- Approval workflows UI (multi-step, quorum)
- Shadow-git checkpoints + full filesystem rollback
- Secret store integration (OS keyring)

### Phase 4 — Learning & Scale
- Learning Engine (evaluate → reflect → extract → version → validate → promote)
- Distributed execution (multiple workers, queue, leader election)
- PostgreSQL persistence backend (behind repository trait — no domain changes)
- Organizational Memory (Honcho or equivalent)

## 5. Explicitly deferred (documented, not silently dropped)

The following spec items are NOT in Phase 1. They are in the roadmap so they aren't lost:

- Learning Engine, skill versioning & promotion
- MCP server hosting
- Wasm/process-sandboxed plugins
- Honcho / Organizational memory
- OpenTelemetry
- Multi-worker distributed execution
- 20+ messaging platform gateways (Hermes already does this well; Forge can integrate
  Hermes gateway or ship API server + call it from external gateways)
- macOS / Linux builds (Phase 1 = Windows only, to minimize CI surface)
- ACP editor integration
- Voice mode, image gen, TTS, browser automation

## 6. Assumption I am running with

**Personal project → build a working Phase-1 vertical slice with clear roadmap. Do not
generate hundreds of stub files for subsystems that will not run.** If you (Sarthak)
want the opposite — architecture docs + shallow skeleton across all 30 subsystems, no
end-to-end runnable path — interrupt and I'll pivot.
