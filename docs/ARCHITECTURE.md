# Forge OS — Phase-1 Architecture

## Bounded contexts / crate layout

```
forge-os/
├── Cargo.toml                 # workspace root
├── crates/
│   ├── forge-domain/          # pure types, no I/O. Zero deps beyond serde/uuid/thiserror/time.
│   ├── forge-events/          # in-process broadcast + persistent append-only event store.
│   ├── forge-persistence/     # sqlx (SQLite) + migrations + repositories.
│   ├── forge-llm/             # LlmProvider trait + adapters + Router (fallback/health).
│   ├── forge-tools/           # Tool trait + built-in tools.
│   ├── forge-policy/          # YAML policy loader + rule evaluator + audit.
│   ├── forge-planner/         # LLM-backed mission → goal DAG decomposition.
│   ├── forge-execution/       # Tokio DAG walker with retries/checkpoints/cancellation.
│   ├── forge-mission/         # facade — create/pause/resume/cancel; wraps planner + execution.
│   └── forge-runtime/         # composition root; boots everything, exposes API to IPC.
├── apps/
│   └── forge-desktop/         # Tauri v2 app
│       ├── src-tauri/         # Rust: registers commands, streams events to webview.
│       └── frontend/          # React + Vite + TS + Tailwind + shadcn + Zustand + TanStack Query + React Flow + Monaco + xterm.js
├── config/
│   ├── policy.default.yaml    # sample policy: require approval for run_command outside allowlist
│   └── forge.toml             # runtime config: LLM provider defaults, workspace paths
└── docs/
    ├── ARCHITECTURE.md        # (this file, published copy)
    └── ROADMAP.md
```

## Dependency graph (crates)

```
                     ┌──────────────────────────┐
                     │       forge-domain       │  (leaf; no deps)
                     └────────────┬─────────────┘
              ┌────────────┬──────┼──────┬──────────────┬──────────┐
              ▼            ▼      ▼      ▼              ▼          ▼
       forge-events forge-persist forge-llm forge-tools forge-policy
              │            │      │           │              │
              └───┬────────┴──────┴─────┬─────┴──────┬───────┘
                  │                     ▼            │
                  │              forge-planner       │
                  │                     │            │
                  └────────────┬────────┴────────────┘
                               ▼
                       forge-execution
                               │
                               ▼
                        forge-mission
                               │
                               ▼
                        forge-runtime
                               │
                               ▼
                     apps/forge-desktop (Tauri)
```

## Domain model (Phase 1)

```rust
pub struct MissionId(Uuid);
pub struct GoalId(Uuid);
pub struct TaskId(Uuid);
pub struct EventId(u64); // monotonic sequence, assigned by event store

pub struct Mission {
    pub id: MissionId,
    pub title: String,
    pub description: String,
    pub status: MissionStatus,        // Draft | Planning | Running | Paused | Completed | Failed | Cancelled
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub goals: Vec<GoalId>,           // materialized via repo
}

pub struct Goal {
    pub id: GoalId,
    pub mission_id: MissionId,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,           // Pending | Ready | Running | Completed | Failed | Skipped
    pub depends_on: Vec<GoalId>,
    pub confidence: f32,
    pub priority: i32,
    pub retries_remaining: u8,
    pub tasks: Vec<TaskId>,
}

pub struct Task {
    pub id: TaskId,
    pub goal_id: GoalId,
    pub tool: String,                 // fully-qualified tool name
    pub input: serde_json::Value,     // schema validated by the tool
    pub status: TaskStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}
```

## Event model

All state transitions are events. Events are:
1. Appended to SQLite `events` table with monotonic `seq` and `aggregate_id`.
2. Broadcast on an in-process `tokio::sync::broadcast` channel.
3. Streamed to the webview via a Tauri `event.emit_all("forge://event", …)`.

```rust
pub enum ForgeEvent {
    MissionCreated { id: MissionId, title: String, ts: OffsetDateTime },
    MissionPlanningStarted { id: MissionId, ts: OffsetDateTime },
    MissionPlanningCompleted { id: MissionId, goal_count: usize, ts: OffsetDateTime },
    MissionStatusChanged { id: MissionId, from: MissionStatus, to: MissionStatus, ts: OffsetDateTime },
    GoalCreated { id: GoalId, mission_id: MissionId, title: String, depends_on: Vec<GoalId>, ts: OffsetDateTime },
    GoalStatusChanged { id: GoalId, from: GoalStatus, to: GoalStatus, ts: OffsetDateTime },
    TaskCreated { id: TaskId, goal_id: GoalId, tool: String, ts: OffsetDateTime },
    TaskCompleted { id: TaskId, result_summary: String, ts: OffsetDateTime },
    TaskFailed { id: TaskId, error: String, ts: OffsetDateTime },
    ToolInvoked { task_id: TaskId, tool: String, input_hash: String, ts: OffsetDateTime },
    PolicyDenied { task_id: TaskId, rule: String, ts: OffsetDateTime },
    PolicyApprovalRequested { task_id: TaskId, rule: String, ts: OffsetDateTime },
    LlmRequested { request_id: String, provider: String, model: String, prompt_tokens: usize, ts: OffsetDateTime },
    LlmResponded { request_id: String, completion_tokens: usize, latency_ms: u64, ts: OffsetDateTime },
}
```

Event store table:
```sql
CREATE TABLE events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    aggregate_id   TEXT    NOT NULL,     -- MissionId or GoalId or TaskId
    aggregate_type TEXT    NOT NULL,     -- 'mission' | 'goal' | 'task'
    event_type     TEXT    NOT NULL,
    payload        TEXT    NOT NULL,     -- JSON
    created_at     INTEGER NOT NULL      -- unix ms
);
CREATE INDEX idx_events_aggregate ON events(aggregate_id, seq);
CREATE INDEX idx_events_created   ON events(created_at);
```

## LLM Router

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn health(&self) -> ProviderHealth;
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
}

pub struct LlmRouter {
    providers: Vec<Arc<dyn LlmProvider>>,
    strategy: RoutingStrategy,       // FailoverInOrder | LowestLatency | Cheapest
    circuit_breakers: DashMap<String, CircuitBreaker>,
}
```

Phase-1 adapters:
- `OpenRouterProvider` (single API key → many models)
- `OllamaProvider` (localhost, no key)
- `MockProvider` (deterministic responses; used by unit tests)

## Tool runtime

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> serde_json::Value;    // JSON Schema for `input`
    fn permissions(&self) -> Vec<Permission>; // e.g. FsRead, FsWrite, Shell
    async fn invoke(&self, ctx: &ToolCtx, input: serde_json::Value) -> Result<serde_json::Value>;
}
```

Built-in tools: `fs.read`, `fs.write`, `fs.list`, `shell.run`, `code.search` (ripgrep-style over workspace).

## Policy engine

`config/policy.default.yaml`:
```yaml
rules:
  - name: deny_shell_outside_workspace
    when: tool == "shell.run"
    check: input.cwd startsWith workspace_root
    on_fail: deny
  - name: approve_destructive_shell
    when: tool == "shell.run" && input.command matches "^(rm|del|format|shutdown)\\b"
    on_fail: require_approval
  - name: deny_fs_outside_workspace
    when: tool startsWith "fs."
    check: input.path startsWith workspace_root
    on_fail: deny
```

Evaluator returns `PolicyDecision::{Allow, Deny(reason), RequireApproval(reason)}` and
writes an audit record for every decision.

## Execution engine

Tokio-based DAG walker:
1. Build `HashMap<GoalId, GoalNode>` from planner output.
2. Compute the set of *ready* goals (all `depends_on` satisfied).
3. Spawn a `tokio::task` per ready goal (bounded by a semaphore for concurrency).
4. Each goal runs its `tasks` sequentially; each task:
   - Emit `ToolInvoked`
   - Ask policy engine
   - If `RequireApproval` → publish event, park until UI approves
   - If `Allow` → execute tool
   - Emit `TaskCompleted` / `TaskFailed`
5. On completion, re-compute ready set. Retry with exponential backoff up to
   `retries_remaining`. On terminal failure, propagate to blocked descendants
   (they become `Skipped`).
6. Cancellation via `CancellationToken`; checkpoint before every tool call so
   resume can pick up mid-DAG.

## Planner

- Input: `Mission { title, description }`, `available_tools: Vec<ToolSchema>`
- Sends a system prompt telling the LLM to output **JSON** matching:
  ```json
  { "goals": [ { "id":"g1", "title":"…", "description":"…", "depends_on":[], "tasks":[{"tool":"fs.read","input":{...}}] } ] }
  ```
- Validates with `jsonschema` crate. On invalid → one retry with the validation error appended.
- Emits `GoalCreated` for each accepted goal and returns a `Plan`.

## Tauri IPC surface (Phase 1)

Commands (Rust → JS):
- `create_mission(title, description) -> MissionId`
- `list_missions() -> Vec<MissionSummary>`
- `get_mission(id) -> MissionDetail` (goals + edges + latest events)
- `cancel_mission(id) -> ()`
- `approve_task(task_id) -> ()`
- `list_providers() -> Vec<ProviderInfo>`

Events (Rust → JS): every `ForgeEvent` is emitted with topic `forge://event`.
Frontend uses Tauri `listen("forge://event", …)` to update Zustand stores;
TanStack Query owns the imperative fetches.

## Frontend structure

```
apps/forge-desktop/frontend/
├── src/
│   ├── main.tsx
│   ├── App.tsx
│   ├── lib/
│   │   ├── ipc.ts             # thin wrappers around Tauri invoke/listen
│   │   └── events.ts          # ForgeEvent type mirror
│   ├── stores/
│   │   ├── missions.ts        # Zustand store
│   │   └── events.ts          # ring buffer of latest N events
│   ├── views/
│   │   ├── MissionList.tsx
│   │   ├── MissionDagView.tsx # React Flow
│   │   ├── EventTimeline.tsx
│   │   └── TaskDetail.tsx     # Monaco for inputs/results, xterm for shell output
│   └── components/
│       └── ui/                # shadcn generated
├── index.html
├── vite.config.ts
├── tailwind.config.ts
└── package.json
```

## Security notes (Phase 1)

- Workspace-scoped tools by policy (fs.* and shell.run cannot escape `workspace_root`)
- Audit trail: every policy decision → event → SQLite
- LLM keys read from OS environment; never persisted to SQLite
- No plugin loading in Phase 1 → attack surface small

Phases 2/3 add: capability-based plugin sandbox, secret store integration, MFA on approvals.

## Testing strategy

- Unit tests co-located per crate.
- `forge-execution` uses `MockProvider` + in-memory event store for deterministic tests.
- Golden JSON fixtures for planner (record real LLM output once, replay).
- `cargo test --workspace` runs everything in <30s.
- Frontend: Vitest for stores/reducers; Playwright deferred to Phase 2.
