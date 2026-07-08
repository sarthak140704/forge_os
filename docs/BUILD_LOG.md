# Forge OS — Build Log

Living reference of what's actually shipped in the repo, kept current every time work completes. Each entry lists the concrete modules, constants, structs, and IPC surfaces you can rely on. If it's not here, it doesn't exist yet.

Related docs:
- `docs/IDEATION.md` — north-star product vision
- `docs/ARCHITECTURE.md` — layer diagram and event flow
- `docs/ROADMAP.md` — what's planned vs shipped (per phase)
- `docs/SKILLS.md` — SKILL.md format
- `docs/MCP.md` — MCP server integration

Discipline: after every checkpoint of real work, update the **matching phase section** and the **Runtime wiring** subsection. Never leave a "TODO in log" — either mark it deferred with a reason or land it.

---

## Repo layout

```
apps/
  forge-desktop/              Tauri v2 shell
    src-tauri/src/lib.rs      IPC commands + AppState + boot_runtime
    frontend/src/             React 18 + Tailwind + Zustand + TanStack Query
crates/
  forge-domain/               Pure types: Mission/Goal/Task IDs + ForgeEvent enum
  forge-events/               Broadcast bus (tokio broadcast) + envelope
  forge-persistence/          SQLite repos (missions/goals/tasks/reflections/events)
  forge-policy/               Declarative Allow/Deny/RequireApproval evaluator
  forge-tools/                Local tool registry (`fs.*`, `shell.run`)
  forge-mcp/                  MCP client (stdio) — spawns servers, adapts tools
  forge-llm/                  Provider trait + Router + circuit breaker
  forge-planner/              LLM-driven plan/replan (JSON schema-constrained)
  forge-execution/            DAG walker, policy check, tool invocation, materializer
  forge-skills/               SKILL.md loader + proposal writer
  forge-mission/              MissionService (plan_and_run, cancel, extend)
  forge-runtime/              Boots everything; owns Runtime struct
config/
docs/
scripts/
```

---

## Phase 1 — Vertical slice (SHIPPED)

Goal: create a mission, plan against real LLM, execute with policy, stream events into a React Flow DAG.

**Key constants / types:**
- `forge_domain::MissionId`, `GoalId`, `TaskId` — newtype `Uuid` wrappers (`"msn_{uuid}"` etc.)
- `forge_domain::ForgeEvent` — every state change (mission/goal/task/tool/policy/llm/mcp/checkpoint)
- `forge_mission::REPLAN_CAP = 5`, `forge_mission::TOTAL_GOAL_CAP = 30`
- `forge_persistence::{EventStore, MissionRepository, GoalRepository, TaskRepository, ReflectionRepository}` traits
- `forge_llm::{LlmProvider, LlmRouter, RoutingStrategy::FailoverInOrder}`
- `forge_policy::{PolicyEngine, Rule, OnFail::{Allow, Deny, RequireApproval}}`

**IPC commands (Phase 1):**
- `create_mission(title, description) -> {id}`
- `plan_and_run(mission_id)`
- `list_missions()`, `get_mission(mission_id)`, `replay_events(since?)`
- `runtime_status()` (readiness probe)

**Runtime wiring** — `crates/forge-runtime/src/lib.rs::Runtime::boot()` composes:
sqlite pool → repos → event bus → tool registry (+ MCP adapters) → LLM router → policy engine → materializer → planner → execution engine → mission service.

---

## Phase 2 — Extensibility (SHIPPED)

- **Skills v2** (`forge-skills`): loads `active/*.md` (agentskills.io front-matter), passes descriptions to the planner. Proposal writer emits `SKILL.md` under `proposed/` for reflector-suggested skills.
- **MCP client** (`forge-mcp`): stdio JSON-RPC. Adapter names tools `mcp.{server}.{tool}` (see `adapter.rs:57`). Configured via `config/mcp.toml`.
- **Just-in-time task materialization** (`forge-execution`): `TaskInputMaterializer` trait. Before each dependent goal runs, re-invokes the LLM with completed upstream results as context, emits `TaskInputRefreshed`.
- **Memory layers**:
  - Working — in-planner recall block
  - Project — `AGENTS.md` / `.forge.md` merged into planner system prompt (`forge_runtime::memory::ProjectMemory`)
  - User — `user.md` under app-data (`forge_runtime::user_memory::UserMemory`)
  - Episodic — keyword recall of prior terminal missions (`forge_runtime::episodic_recall::extract_keywords`, `RecallSurface { block, keywords, prior_count }`)
- **Cost tracking**: `LlmRequested`/`LlmResponded`/`LlmFailed` events + `MissionCostSummary` roll-up on terminal.
- **Feature flags** (`forge_runtime::feature_flags`): `feature-flags.toml` under app-data; env override `FORGE_FLAG_*`. Structs: `MaterializerFlag`, `EpisodicRecallFlag { enabled, max_recall }`, `CostSummaryFlag`.
- **Cancellation IPC**: `cancel_mission(mission_id)` emits `MissionCancelRequested` before flipping the cooperative token.
- **Mission extension IPC**: `extend_mission(mission_id, prompt)` appends `### Follow-up request` and re-runs `plan_and_run` (Terminal → Draft transition).
- **Skill promotion IPC**: `list_skill_proposals`, `approve_skill_proposal(filename)`, `reject_skill_proposal(filename)`. `list_reflections(mission_id)` for post-mortem view.
- **Providers**: Groq (default), OpenRouter, OpenAI, Ollama, Mock. Key env vars: `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `FORGE_LLM_MODEL`.

**Deferred**: Semantic memory (sqlite-vss), OTel exporter, macOS/Linux CI, Anthropic/Gemini/Azure/vLLM providers.

---

## Phase 3 — Governance (SHIPPED except sandbox + quorum)

### Shadow-git checkpoints — `crates/forge-runtime/src/checkpoints.rs`

- **Location**: `<app-data>/checkpoints/.git` (sibling to `forge.sqlite`)
- **Model**: shadow repo = git-dir; workspace = work-tree via `--git-dir=<x>/.git --work-tree=<ws>`
- **Author**: synthetic `Forge OS <forge@localhost>` (GIT_AUTHOR_* + GIT_COMMITTER_* env)
- **Config baked in on init**:
  - `core.bare=false` (created via `git init --bare` then flipped so add/commit work)
  - `core.worktree=<workspace>`
  - `core.autocrlf=false`, `core.safecrlf=false` (**critical** on Windows; without them `git add` refuses CRLF files)
- **Trailers**: `Forge-Mission-Id:`, `Forge-Task-Id:`, `Forge-Tool:`
- **Public API**:
  - `pub struct Checkpoint { sha, short_sha, subject, timestamp, mission_id, task_id, tool, files_changed, insertions, deletions }`
  - `pub struct CheckpointStore { workspace, git_dir, lock: Mutex, enabled }`
    - `init(workspace, git_dir) -> Self` (best-effort; disabled if git missing)
    - `commit(label, mission_id?, task_id?, tool?) -> Result<Option<String>, String>` — `None` = empty diff (no commit)
    - `list(limit, mission_id_filter?) -> Vec<Checkpoint>` — parses `git log --format='\x1e%H\x1f%h\x1f%s\x1f%aI\x1f%b' --shortstat`. **Record separator LEADS the format**, not trails — otherwise the shortstat block that git prints AFTER each commit spills into the next record's leading text and files_changed/insertions/deletions all read 0.
    - `revert(sha) -> Result<(), String>` — `git reset --hard <sha> && git clean -fd` (destructive)
    - `is_enabled() -> bool`
- **Auto-snapshot**: `Runtime::boot()` spawns a task subscribing to the event bus. On `ForgeEvent::TaskCompleted` looks up task → tool via `TaskRepository::get()`, then goal → mission via `GoalRepository::get()`. If `is_mutating_tool(&tool)` returns true → `cp.commit(...)`:
  - `Ok(Some(sha))` → emit `ForgeEvent::CheckpointCreated { sha, short_sha, tool, mission_id, task_id, label }`
  - `Ok(None)` (no workspace changes — e.g. identical bytes re-written) → emit `ForgeEvent::CheckpointSkipped { tool, mission_id, task_id, reason }` so the timeline shows explicit "no-op" feedback instead of silently swallowing the attempt.
- **`is_mutating_tool(name)`** — hot path filter:
  - Local: `fs.write | fs.mkdir | fs.append | fs.delete | fs.move | shell.run`
  - MCP: `mcp.*` where name contains `write | create_directory | edit | move | delete | append | mkdir | rename`
  - **Actual tool names in registry**: `fs.read`, `fs.write`, `fs.mkdir`, `fs.list`, `shell.run` (`crates/forge-tools/src/builtins.rs`) and `mcp.<server>.<remote>` (`crates/forge-mcp/src/adapter.rs:57`). Do NOT match `file_write` / `create_directory` — those aren't real names.
- **Tests**: `checkpoints::tests::{empty_commit_returns_none, init_and_commit_and_list_roundtrip, list_populates_files_and_line_counts, revert_restores_file_contents}` — all 4 pass, skip on machines without git.
- **Headless smokes** (no LLM tokens spent, no UI required):
  - `cargo run -p forge-runtime --example checkpoints_headless_smoke` — injects synthetic mission/goal/task via `Runtime::{goals,tasks}` public repos and publishes `TaskCompleted` directly. Asserts `CheckpointCreated` fires on new content and `CheckpointSkipped` fires on duplicate content.
  - `cargo run -p forge-runtime --example checkpoints_smoke` — full end-to-end via the planner LLM; useful when the Groq quota isn't exhausted.
- **Runtime exposes** `pub goals: Arc<SqliteGoalRepository>` and `pub tasks: Arc<SqliteTaskRepository>` so headless drivers and integration tests can seed the DB without going through the planner.
- **IPC**: `list_checkpoints(mission_id?, limit?) -> Vec<Checkpoint>`; `revert_checkpoint(sha)`.

### Secrets — `crates/forge-runtime/src/secrets.rs`

- **Backend**: `keyring` crate v3, features `windows-native,apple-native,linux-native-sync-persistent`
- **Service const**: `SERVICE = "com.sarthak.forgeos"` (matches app-data folder)
- **Public API**:
  - `pub fn set(name, value) -> Result<(), String>`
  - `pub fn get(name) -> Option<String>`
  - `pub fn has(name) -> bool`
  - `pub fn delete(name) -> Result<(), String>`
  - `pub fn resolve(name) -> Option<String>` — **env var wins over keyring** (backward-compat)
  - `pub const KNOWN_SECRETS = ["GROQ_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"]`
- **Runtime integration**: `boot_runtime()` calls `secrets::resolve()` for each known key and bridges into `std::env::set_var(...)` so downstream provider clients (still env-based) pick them up transparently.
- **IPC**: `list_secret_status() -> Vec<{name, set, source: "env"|"keyring"|"unset"}>`; `set_secret(name, value)`; `delete_secret(name)`.

### Audit export — `crates/forge-runtime/src/audit.rs`

- **Schema v1**: JSON `{ schema_version: 1, exported_at, missions[], goals[], tasks[], events[], reflections[] }`
- **Row serialization**: dynamic reflection on `SqliteColumn`; type-guess order i64 → f64 → String → hex(Vec<u8>) → Null
- **Public API**:
  - `pub struct AuditBundle`, `pub struct Counts { missions, goals, tasks, events, reflections }`
  - `AuditBundle::build(&SqlitePool) -> Result<Self, String>`
  - `write_to(&SqlitePool, &Path) -> Result<Counts, String>`
- **IPC**: `export_audit(dest) -> {path, missions, goals, tasks, events, reflections}` — front-end uses `@tauri-apps/plugin-dialog::save` for file picker.

### Deferred (Phase 3.5 / 4)

- Wasmtime plugin sandbox
- Multi-step / quorum approval workflows
- OTel exporter
- Cross-platform CI

---

## Phase 4a — Version-controlled skills (SHIPPED)

**Motivation (agent.txt):** *"Every learned improvement should be version-controlled. Nothing should ever be overwritten. Everything should be reversible."* Phase 4a lands the append-only history + content-addressed store + curator.

### Persistence — `crates/forge-persistence`

- **Migration `V002_SKILLS_HISTORY`** (`migrations.rs`, applied automatically in `connect()`). Creates:
  ```sql
  CREATE TABLE skills_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    sha  TEXT NOT NULL,
    version TEXT NOT NULL,
    origin TEXT NOT NULL,          -- proposal|handcrafted|rollback|curated
    origin_mission_id TEXT NULL,
    parent_sha TEXT NULL,
    promoted_at TEXT NOT NULL,
    retired_at  TEXT NULL,
    reason TEXT NULL
  );
  CREATE INDEX idx_skills_history_name     ON skills_history (name, id);
  CREATE INDEX idx_skills_history_sha      ON skills_history (sha);
  CREATE INDEX idx_skills_history_promoted ON skills_history (promoted_at);
  ```
- **Types** (`lib.rs`): `SkillOrigin` enum, `SkillVersionRecord`, `NewSkillVersion`, trait `SkillHistoryRepository` with `promote/retire_active/active/history/list_active`.
- **Impl** (`sqlite.rs`): `SqliteSkillHistoryRepository::new(pool)`. Currently-active for `name` = newest row with `retired_at IS NULL`. Rows are **never** mutated.

### Content store — `crates/forge-skills/src/versions.rs`

- **`SkillVersionStore::new(<skills_root>)`** — sharded `<root>/history/<3-char-shard>/<64-char-sha>.md`.
- `hash(bytes)` → hex SHA-256; `put(sha, bytes)` — idempotent (skips write if the file already exists); `get(sha)` → bytes; `contains(sha)` → bool.
- 3 unit tests: hash stability, put/get roundtrip, missing-sha error path.

### Orchestration + curator — `crates/forge-runtime/src/skills_ops.rs`

- **`SkillOps`** — owns `skills_root`, `history` repo, `store`, `events`.
  - `promote_from_proposal(filename, origin_mission_id) -> SkillVersionRecord` — approves the proposal file, snapshots bytes into the content store, retires prior active row (if any), appends new row with `parent_sha` = prior sha, publishes `SkillPromoted`.
  - `retire(name, reason) -> Option<String>` — moves ALL matching files in `active/` to `archived/`, sets `retired_at`, publishes `SkillRetired`. Returns the sha it retired (or `None` if nothing was active).
  - `rollback(name, target_sha, reason) -> SkillVersionRecord` — restores bit-exact bytes from the content store to `active/<name>.md`, retires prior active row, appends a new `origin=rollback` row whose `sha == target_sha` and `parent_sha` = the sha it displaced. Publishes `SkillRolledBack`.
  - `seed_missing_history()` — called at boot; every parseable file in `active/` without a matching history row is snapshotted with `origin=handcrafted`. Also catches on-disk edits (same name, different bytes → retire old + promote new).
- **`Curator`** — advisory heuristics, no LLM:
  - **Duplicate**: pairwise Jaro-Winkler on active names, threshold `≥ 0.90`.
  - **Unused**: scans the event log for `SkillsSelected`; any active skill whose name never appears is flagged.
  - Every finding also publishes `SkillCurationSuggested`.
- **Retire-all fix** (`forge-skills/proposal.rs`): `retire_active_skill` now archives *every* file in `active/` whose front-matter name matches, disambiguating archived-dir collisions by `<n>-<filename>` prefix. Prior single-file version left stale copies after rollback.

### Events (`crates/forge-domain/src/event.rs`)

Four new variants — all classified as `AggregateKind::Skill`:
- `SkillPromoted { name, sha, version, origin, parent_sha, origin_mission_id }`
- `SkillRolledBack { name, from_sha, to_sha, reason }`
- `SkillRetired { name, sha, reason }`
- `SkillCurationSuggested { name, kind, evidence }`

### IPC (`apps/forge-desktop/src-tauri/src/lib.rs`)

Five new commands + `SkillVersionDto`:
- `list_active_skills() -> Vec<SkillVersionDto>`
- `list_skill_versions(name) -> Vec<SkillVersionDto>` (newest first, includes retired rows)
- `rollback_skill(name, sha, reason?) -> SkillVersionDto`
- `retire_skill(name, reason) -> Option<String>` (returns retired sha)
- `run_curator() -> Vec<CuratorSuggestion>`
All fail loudly if `skills_root` isn't configured on the runtime.

### Runtime wiring

`Runtime` gains two optional fields (`skill_ops`, `curator`), both `Some(_)` when `RuntimeConfig.skills_root` is set. Boot calls `seed_missing_history().await` before returning so on-disk files show up in the history table without a full round-trip through the proposal flow.

### Verify

- `cargo test -p forge-persistence -p forge-skills -p forge-runtime` → 46 pass (24 runtime + 22 skills + 0 persistence unit)
- `cargo run -p forge-runtime --example skill_versioning_smoke` → 6 scenarios pass end-to-end (promote → promote → rollback → history assertions → retire → curator duplicate detection)

### Deferred to Phase 4b

- Frontend Settings > Skills tab (list, diff viewer, one-click rollback, retire, curator panel) — commands land now, UI polish comes with the next visible-user-value drop.
- Curator auto-promotion (currently advisory only; human still runs it).
- Postgres backend for `SkillHistoryRepository` — trait-boundary is ready.

---

## Frontend surface

**Stores** (`apps/forge-desktop/frontend/src/stores/`)
- `useUiStore` — `selectedMissionId`, `select(id)`
- `useEventsStore` — event replay buffer, builds `goalToMission` + `taskToGoal` indices from `goal_created`/`task_created`

**IPC wrappers** (`lib/ipc.ts`) — one function per Tauri command, plus:
- `subscribeEvents(cb)`, `subscribeRuntimeReady(cb)`, `subscribeRuntimeError(cb)`
- Types: `Checkpoint`, `SecretStatus`, `AuditExportResult`

**Event helpers** (`lib/event-filter.ts`) — `eventMissionId(ev, g2m, t2g)`, `eventCategory(ev)` returning `mission|goal|task|llm|plugin|meta`, `filterEvents({missionId, categories, query, ...})`.

**Views** (`views/`)
- `CreateMission` — form; auto-selects new mission after `createMission()` so DAG opens
- `MissionList` — sidebar list
- `MissionDagView` — React Flow. Cancel + Follow-up (extend) buttons in header. MiniMap removed; Controls at bottom-left, themed via `.forge-flow` CSS in `index.css`.
- `EventTimeline` — right sidebar. Mission filter (default: current only) + 6 category chips + free-text search. All event types have summarize cases.
- `Settings` — modal (⚙ in header). 3 sections:
  - **Secrets** — env/keyring/unset badges; Set/Replace/Delete per key
  - **Checkpoints** — scoped to current mission; Revert with confirm
  - **Audit** — file picker → JSON dump; success line shows row count

---

## Boot + verify

```powershell
$env:GROQ_API_KEY = (Get-Content C:\Users\t-sarverma\Downloads\key.txt -Raw).Trim()
cd C:\Users\t-sarverma\Projects\forge-os\apps\forge-desktop
node .\frontend\node_modules\@tauri-apps\cli\tauri.js dev --config .\src-tauri\tauri.conf.json
```

Expected boot lines:
```
INFO forge_runtime: mcp server started mcp=filesystem tools=14
INFO forge_runtime: feature flags loaded materializer=true episodic_recall=true cost_summary=true
INFO forge_runtime: loaded skills count=<N>
INFO forge_runtime: forge runtime booted
INFO forge_runtime: shadow-git checkpoints enabled     <- Phase 3
INFO forge_desktop_lib: runtime ready, IPC commands live
```

App data locations (Windows):
- SQLite: `%APPDATA%\com.sarthak.forgeos\forge.sqlite`
- Workspace: `%APPDATA%\com.sarthak.forgeos\workspace`
- Shadow git: `%APPDATA%\com.sarthak.forgeos\checkpoints\.git`
- Skills: `%APPDATA%\com.sarthak.forgeos\skills\{active,proposed}`
- Feature flags: `%APPDATA%\com.sarthak.forgeos\feature-flags.toml`

Verify commands:
```powershell
# List distinct tool names seen in event stream
python scripts\list_tools.py

# Dump events with per-mission tool trace
python scripts\inspect_tools.py

# Confirm shadow-git commits are landing
git --git-dir="$env:APPDATA\com.sarthak.forgeos\checkpoints\.git" log --oneline -n 20

# Full workspace check
cargo check --workspace
cargo test  -p forge-runtime checkpoints
cd apps/forge-desktop/frontend; npx tsc --noEmit
```

---

## Known gotchas

- **Windows CRLF** — shadow-git `add` fails without `core.autocrlf=false` + `core.safecrlf=false`. Baked into `CheckpointStore::init` on first-ever init only; if you have an OLD shadow repo, apply manually: `git --git-dir=<path> config core.autocrlf false; git --git-dir=<path> config core.safecrlf false`.
- **Groq TPM** — `llama-3.3-70b-versatile` free tier caps at 12k tokens/minute. Reflection call on rapid missions can hit HTTP 429; mission itself completes fine.
- **No `sqlite3` CLI on default Windows** — use Python `sqlite3` module (see `scripts/*.py`).
- **Tauri launch** — always from `apps/forge-desktop` running `node .\frontend\node_modules\@tauri-apps\cli\tauri.js dev --config .\src-tauri\tauri.conf.json`. `cargo tauri dev` and `npm run tauri` variants have proven flaky.
- **PowerShell fresh-process PATH** — each `powershell(...)` call needs `$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User');` prefix.
