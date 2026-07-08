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

## Phase 4b — Validation gate + AutoPromoter + Skills UI (SHIPPED)

**Motivation (agent.txt):** *"Learning should require validation before promotion."* Phase 4b lands the gate + a background sweeper + the Settings > Skills panel (closing the deferred 4a-frontend slice).

### Validator — `crates/forge-skills/src/validate.rs`

- **`SkillValidator::new(ctx)`** with `ValidatorContext { known_tools, active_skills }`.
- **`ValidationReport { ok, checks: Vec<ValidationCheck> }`** — `ValidationCheck { id, level: Hard|Soft, passed, message }`. `ok` is true iff every **hard** check passes; soft failures do not gate promotion.
- **5 hard checks:**
  - `parses` — YAML front-matter deserializes
  - `body_length` — `body.split_whitespace().map(str::len).sum() >= 40`
  - `has_trigger` — non-empty keywords **or** file_globs
  - `tools_declared` — tools list non-empty
  - `tools_resolvable` — every tool either in `known_tools` **or** starts with `mcp:` (MCP tools are always accepted; policy gate handles them at runtime)
- **3 soft checks:**
  - `no_name_collision` — ≥3-keyword overlap with any **other** active skill (using `active_skills`)
  - `version_monotonic` — same-named active skill has higher semver (3-component `parse3`)
  - `keywords_normalised` — any uppercase keyword (matcher is case-insensitive; warn only)
- 10 unit tests cover each check (`cargo test -p forge-skills validate::`).

### SkillOps integration — `crates/forge-runtime/src/skills_ops.rs`

- **`SkillOps::with_tools(...)`** — new constructor that takes `known_tools: Vec<String>` (fed by `ToolRegistry::names()`). The plain `SkillOps::new` still works with an empty whitelist for isolated tests.
- **`SkillOps::validate_proposal(filename)`** — public; loads the file, snapshots active-skill keywords by re-scanning `active/` (bounded ≤50), runs the validator, returns `ValidationReport`.
- **`promote_from_proposal` gate:**
  1. assert `proposed/<filename>` exists
  2. `validate_proposal(filename)`
  3. if `!report.ok` → publish `SkillValidationFailed { filename, name, failed_checks }` + return `SkillOpsError::ValidationFailed { filename, failed }`; file left in `proposed/`
  4. else publish `SkillValidationPassed { filename, name, soft_failures }` and continue original 4a promotion flow

### AutoPromoter — `crates/forge-runtime/src/skills_ops.rs` (bottom)

- **`AutoPromoter::new(ops, events, interval)`** + **`Arc<AutoPromoter>::spawn(self)`** — background tokio loop with `MissedTickBehavior::Delay` (no stampede on slow sweeps). First tick is skipped (boot already ran `seed_missing_history`).
- **`sweep() -> Vec<String>`** — enumerates `proposed/`, validates each, promotes passing ones, publishes `SkillAutoPromoted { name, sha, version }` (distinct from human-approved `SkillPromoted`).
- Off by default via `RuntimeConfig.auto_promote_skills = false`; interval clamped to `max(30, autopromote_interval_secs)` seconds.

### Events (`crates/forge-domain/src/event.rs`)

Three new variants, all `AggregateKind::Skill`:
- `SkillValidationPassed { filename, name, soft_failures }`
- `SkillValidationFailed { filename, name, failed_checks }`
- `SkillAutoPromoted { name, sha, version }`

### Runtime + Tool wiring

- `crates/forge-tools/src/lib.rs`: added `ToolRegistry::names() -> Vec<String>` (used to seed the validator whitelist).
- `RuntimeConfig` extended: `auto_promote_skills: bool` (default false, `#[serde(default)]`), `autopromote_interval_secs: u64` (default 300).
- `Runtime::boot` now passes `tools.names()` to `SkillOps::with_tools(...)` and spawns the `AutoPromoter` when `auto_promote_skills = true`.

### IPC (`apps/forge-desktop/src-tauri/src/lib.rs`)

- New command **`validate_skill_proposal(filename) -> ValidationReport`** (returns the report as-is; `ValidationReport` derives `Serialize`).
- Registered in `invoke_handler`; existing 4a commands unchanged.

### Frontend — Settings > Skills

- **`SkillsSection`** in `views/Settings.tsx` (~200 LOC total across three components):
  - **`ProposalRow`** — per-proposal card: Validate button renders green/red per-check badges; Approve is disabled until validation passes; Reject drops the file.
  - **`ActiveSkillRow`** — expands to full history (proposal / rollback / handcrafted origin badges); one-click Rollback (with reason prompt) and Retire (with reason).
  - Curator suggestions listed underneath with kind badges.
- **`lib/ipc.ts`** — 9 wrappers: `listSkillProposals`, `approveSkillProposal`, `rejectSkillProposal`, `validateSkillProposal`, `listActiveSkills`, `listSkillVersions`, `rollbackSkill`, `retireSkill`, `runCurator`. Types added: `SkillProposalSummary`, `SkillVersion`, `CuratorSuggestion`, `ValidationCheck`, `ValidationReport`.
- **`lib/events.ts` + `event-filter.ts` + `views/EventTimeline.tsx`** — new event variants added, categorized under `meta`, with summarize cases (per-event human-readable line).

### Verify

- `cargo test -p forge-persistence -p forge-skills -p forge-runtime` → **56 pass** (24 runtime + 32 skills = 22 old + 10 new validator)
- `cargo run -p forge-runtime --example skill_validation_smoke` → 3 scenarios PASS: good→promote, bad→ValidationFailed(`body_length`,`has_trigger`,`tools_resolvable`), AutoPromoter.sweep() picks up a fresh good one and emits `SkillAutoPromoted`
- `cargo run -p forge-runtime --example skill_versioning_smoke` → 6/6 scenarios still pass (bodies extended to satisfy the new `body_length` gate; tools/keywords added to satisfy `tools_declared`/`has_trigger` — this is exactly the behaviour we want from the validator)
- Frontend `npx tsc --noEmit` → clean

---


## Phase 4c — Actionable Skill Curator (SHIPPED)

**Motivation (agent.txt):** *"Learning without curation drifts."* Phase 4c elevates the previously-advisory Curator to an actionable one: it auto-archives near-duplicates, drops merge proposals into `proposed/` (validator-gated), and still surfaces "unused" advisory rows — with a recent-usage protection window so freshly-added skills aren't flagged after two missions.

### Similarity math — `crates/forge-skills/src/similarity.rs` (NEW)

Pure functions, no I/O, no runtime deps. 9 unit tests cover identical / disjoint / paraphrase / subset / merge.

- `tokenize(s)` — lowercase, split on non-alphanumeric, keep tokens with `len ≥ 3`.
- `shingles(tokens, 3)` — 3-gram shingles with fallback to individual tokens for very short bodies.
- `jaccard(a, b)` — set Jaccard on shingles. `jaccard(∅, ∅) = 1.0`; `jaccard(∅, x) = 0.0`.
- `body_similarity(a, b)` — Jaccard on token-3-grams of the two bodies.
- `subset_ratio(a, b)` — fraction of the *smaller* body's shingles present in the larger. Detects containment when Jaccard understates it.
- `merge_bodies(a, b)` — keeps longer verbatim, appends paragraphs from shorter not already substring-present, with a `<!-- merged: ... -->` marker.
- `union_dedup([...])` — order-preserving dedup.

### Curator — `crates/forge-runtime/src/skills_ops.rs` (rewritten)

- **`CuratorPolicy`** (`#[serde(default)]`):
  - `name_similarity_threshold = 0.92`
  - `body_similarity_threshold = 0.85`
  - `subset_ratio_threshold = 0.95`
  - `merge_similarity_low = 0.35`
  - `recent_mission_window = 20`
- **`CuratorReport { suggestions, auto_archived, merge_proposals }`** — actions taken are always separate from suggestions so UIs can render "we found N *and* did K".
- **`Curator::with_policy(pool, root, events, policy)`** + **`scan(apply: bool) -> CuratorReport`**. Legacy `Curator::new` + `run()` preserved for backwards compat.
- **Auto-archive rules** (any triggers): `name_sim ≥ threshold` OR `body_sim ≥ threshold` OR `subset_ratio ≥ threshold`.
- **Merge band**: `body_sim ∈ [merge_low, body_threshold)` — writes a merged file to `proposed/` with a computed `merged_name_of(a, b)` (sorted so orderings collapse); publishes `SkillMergeProposed`.
- **Deterministic loser pick** (`pick_loser`): if either skill was in the last N terminal missions, protect it; otherwise the alphabetically-later name loses. Prevents non-determinism across runs.
- **Recent-usage window** (`recently_used_names`): picks the most-recent N *terminal* missions (from `MissionStatusChanged` where `to ∈ completed|failed|cancelled`), collects `SkillsSelected` names. **Fallback:** if fewer terminal missions exist than N, treat ALL missions as "recent" — prevents new users from having every skill flagged as unused after 2 runs.
- **Idempotency** (`pending_proposal_names`): scans `proposed/` and skips merge proposals whose `merged_name` already exists there. Second scan is a no-op.

### CuratorSweeper — bottom of `skills_ops.rs`

Mirrors `AutoPromoter`. Background tokio loop with `MissedTickBehavior::Delay`. Off by default via `RuntimeConfig.curator_sweep_enabled = false`; interval clamped to `max(60, curator_interval_secs)`.

### Events (`crates/forge-domain/src/event.rs`)

Two new variants, both `AggregateKind::Skill`:
- `SkillAutoArchived { name, reason, similarity, kept }` — emitted when Curator moves a duplicate to `archived/`.
- `SkillMergeProposed { filename, merged_name, sources, similarity }` — emitted when a merge proposal file lands in `proposed/`. Validator (Phase 4b) then gates promotion.

### LLM error surfacing — `crates/forge-llm/src/lib.rs`

Independent fix that landed alongside Phase 4c: `LlmChain::complete()` now aggregates *every* provider's error into `LlmError::AllFailed("[groq] <msg> | [ollama] <msg>")` instead of only the last one. Root-caused after a Groq TPD-exhaustion cascade was hidden behind an Ollama connection error.

### Runtime + IPC

- **`RuntimeConfig`**: `curator: CuratorPolicy` (default), `curator_sweep_enabled: bool` (default false), `curator_interval_secs: u64` (default 900). Boot swaps `Curator::new` → `Curator::with_policy` and spawns `CuratorSweeper` when enabled.
- **New IPC command `curator_scan(apply: bool) -> CuratorReport`** in `apps/forge-desktop/src-tauri/src/lib.rs`. Registered in `invoke_handler`.

### Frontend — Settings > Skills

`SkillsSection` in `views/Settings.tsx` upgraded with three buttons:
- **Advisory scan** — `curator_scan(false)` filtered to suggestions only.
- **Dry-run scan** — `curator_scan(false)` — full report incl. would-be archive/merge classifications.
- **Scan & apply** — `curator_scan(true)` — auto-archives + writes merge proposals. Renders an actions-taken panel: archived skills + merge-proposal filenames.

Types + IPC binding added in `lib/ipc.ts` (`CuratorReport`, `curatorScan`). Event union + `event-filter.ts` + `EventTimeline.tsx` summarize cases added for the two new events.

### Verify

```powershell
cargo test -p forge-skills --lib similarity                # 9/9 pass
cargo run -p forge-runtime --example skill_curator_smoke   # 3/3 scenarios pass
cargo test --workspace                                     # existing baseline preserved (except pre-existing user_memory env-var race in --test-threads>=2)
```

The smoke covers dry-run classification, apply (dedupe + merge + validator OK), and idempotent 2nd pass (0 new proposals).

---


## Phase 4d — Distributed / Worker Pool (SHIPPED)

**Motivation (agent.txt):** *"Missions must survive restarts and run concurrently."* Phase 4d puts a persisted queue in front of the executor so the UI can enqueue N missions and workers drain them in parallel — with crash recovery, backpressure visibility, and honest scope (in-process workers, not networked; that's Phase 5).

### Schema — migration V003 (`crates/forge-persistence/src/migrations.rs`)

```sql
CREATE TABLE mission_queue (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  mission_id   TEXT    NOT NULL,
  status       TEXT    NOT NULL,        -- queued|claimed|done|failed
  claimed_by   TEXT,
  claimed_at   TEXT,
  heartbeat_at TEXT,
  finished_at  TEXT,
  error        TEXT,
  enqueued_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_queue_status ON mission_queue(status);
CREATE INDEX idx_queue_mission ON mission_queue(mission_id);
```

Multiple rows per `mission_id` are allowed (retries after terminal state); at most one Queued/Claimed row per mission is enforced at the `enqueue` call site.

### Queue trait — `crates/forge-persistence/src/lib.rs`

`MissionQueueRepository`:
- `enqueue(mid)` — idempotent on ACTIVE dupes; terminal rows can be re-enqueued as fresh retry rows.
- `claim_next(worker_id)` — two-step SELECT-then-UPDATE transaction. Returns `None` on lost race → worker loops.
- `heartbeat(id)` — no-op if the row moved (worker got kicked off).
- `finish(id, success, error)` — terminal transition.
- `requeue_stale(secs)` — rescue any Claimed row whose heartbeat is older than `secs` (or was never heartbeated and claimed more than `secs` ago).
- `depth() -> (queued, claimed)`, `recent(n)` — observability.

`SqliteMissionQueueRepository` at the bottom of `sqlite.rs` with 4 unit tests (enqueue idempotency, claim/finish, stale requeue, depth/recent).

### WorkerPool — `crates/forge-runtime/src/worker_pool.rs` (NEW)

- N tokio worker tasks (`workers: N` in `RuntimeConfig`, default 0 = disabled). Each loops:
  `claim_next → spawn heartbeat task → plan_and_run_sync → abort heartbeat → finish(success)`.
- Janitor task runs `requeue_stale(stale_after_secs)` every `stale_after_secs / 2`.
- Per-mission heartbeat task fires every `stale_after_secs / 3`.
- Head comment documents "why not just tokio::spawn" (needs persistence + observability) and "why not networked distributed" (personal-desktop scope; Phase 5).

### MissionService integration (`crates/forge-mission/src/lib.rs`)

- New public methods:
  - `enqueue(id)` — inserts into queue, publishes `MissionQueued`. Called by the IPC layer (currently only via the smoke; no UI enqueue button yet — planned Phase 5).
  - `plan_and_run_sync(id)` — plan + execute-and-reflect inline (blocking). WorkerPool calls this.
- Existing `plan_and_run(id)` — kept as a `tokio::spawn` wrapper for backward compat. IPC still uses it so `plan_and_run` returns immediately.
- Two shared helpers (`plan_only`, `execute_and_reflect`) factor the sync/async paths cleanly.
- When `workers == 0` (default), MissionService's `queue: None` and everything behaves exactly like Phase 3.

### Boot flow (`crates/forge-runtime/src/lib.rs`)

1. Build queue + org_memory repos (always) and expose on `Runtime`.
2. If `workers > 0`:
   - Wire queue into MissionService.
   - Run `queue.requeue_stale(worker_stale_secs)` once at boot (crash recovery).
   - Spawn `WorkerPool::new(...).start()`.

### Events (`crates/forge-domain/src/event.rs`)

- `MissionQueued { mission_id, queue_id }` — fired by `MissionService::enqueue`.

### IPC + UI

- New command `queue_status()` → `{workers, queued, claimed, recent: [MissionQueueRow]}`.
- `Settings > Mission Queue` section:
  - Badge: `N workers` (green) or `inline mode` (blue).
  - Live `queued` + `claimed` counts (auto-refresh every 4s).
  - Recent 20 rows, colour-coded by status: `done`=green, `failed`=red, `claimed`=amber, `queued`=blue.
- Frontend event union + `event-filter.ts` + `EventTimeline.tsx` updated for `mission_queued`.

### Verify

```powershell
cargo test -p forge-persistence                            # 6/6 pass (2 queue + 2 memory + 2 postgres stub)
cargo run -p forge-runtime --example worker_pool_smoke     # 3/3 scenarios pass
```

The smoke enqueues 4 missions with `workers=2`, drains them, then simulates a crashed worker (backdate heartbeat) and verifies `requeue_stale` rescues it within one janitor tick.


## Phase 4e — Postgres Backend Scaffold (SHIPPED — trait boundary only)

**Motivation:** cleanly separate SQLite specifics from the mission domain so a future Postgres backend is a build-time swap, not a refactor.

### `PersistenceHandles` — `crates/forge-persistence/src/lib.rs`

```rust
pub struct PersistenceHandles {
    pub pool_kind: PoolKind,   // Sqlite | Postgres
    pub events:      Arc<dyn EventStore>,
    pub missions:    Arc<dyn MissionRepository>,
    pub goals:       Arc<dyn GoalRepository>,
    pub tasks:       Arc<dyn TaskRepository>,
    pub reflections: Arc<dyn ReflectionRepository>,
    pub skills:      Arc<dyn SkillHistoryRepository>,
    pub queue:       Arc<dyn MissionQueueRepository>,
    pub memory:      Arc<dyn OrgMemoryRepository>,
    pub sqlite_pool: Option<SqlitePool>,  // for shadow-git / raw SQL only
}
```

- `PersistenceHandles::sqlite(url)` — normal boot path.
- `PersistenceHandles::postgres(url)` — currently returns `PersistenceError::NotYetImplemented("postgres persistence backend")`.
- `PersistenceHandles::open(url)` — dispatches by URL scheme: `postgres://` or `postgresql://` → `postgres()`, everything else → `sqlite()`.

### Honest stub — `crates/forge-persistence/src/postgres.rs` (NEW)

Validates the URL shape and returns `NotYetImplemented`. Head comment contains the concrete 5-step Phase 5 rollout plan (add `sqlx-postgres`, translate migrations, port each Sqlite repo, wire on `open()`, add integration test with docker-compose).

### `PersistenceError::NotYetImplemented(&'static str)`

New variant, used by the postgres stub and reserved for future capability-gated features.

### Runtime wiring

The Runtime doesn't yet route through `PersistenceHandles::open` — it still constructs SQLite repos directly. The `PersistenceHandles` bundle is a public swap boundary for external callers and future refactors. This keeps the Phase 4e diff scoped.

### Verify

```powershell
cargo run -p forge-runtime --example postgres_dispatch_smoke   # 3/3 scenarios pass
```

Confirms `sqlite://` opens successfully with a working queue repo, `postgres://` returns the honest error, and unknown schemes are rejected downstream by the SQLite validator.


## Phase 4f — Organizational Memory (SHIPPED)

**Motivation (agent.txt):** *"The system should get smarter across missions, not just within one."* Phase 4f captures durable cross-mission insights and injects them into every planner prompt as a third memory layer alongside episodic (per-mission history) and project (workspace facts).

### Schema — migration V004

```sql
CREATE TABLE org_memory (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  key               TEXT    NOT NULL,     -- one-line insight headline
  value             TEXT    NOT NULL,     -- full insight body
  tags              TEXT    NOT NULL,     -- JSON array of lowercased tokens
  source_mission_id TEXT,                 -- which mission produced it
  created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
  retired_at        TEXT                  -- soft-delete via UI
);
CREATE INDEX idx_orgmem_active ON org_memory(retired_at) WHERE retired_at IS NULL;
```

### Trait — `crates/forge-persistence/src/lib.rs`

`OrgMemoryRepository`:
- `insert(&NewOrgMemory) -> id`
- `retire(id) -> bool` (idempotent; second call returns false).
- `list_active(limit) -> Vec<OrgMemoryRow>` (newest first).
- `search(keywords: &[String], limit) -> Vec<OrgMemoryRow>` — case-insensitive LIKE across key/value/tags, scored by match count. MVP; a real semantic recall (sqlite-vss) is Phase 5.

### Extractor — `crates/forge-mission/src/lib.rs`

**Zero extra LLM cost.** `reflect_and_learn` already produces `reflection.insights: Vec<String>` per mission. Phase 4f persists each entry as an `org_memory` row with:
- `key` = insight text (first ~120 chars).
- `value` = insight text (full).
- `tags` = mission title keywords (`keyword_extract`) + top-3 selected skill names.
- `source_mission_id` = the reflecting mission.

Each write publishes `OrgMemoryLearned { mission_id, memory_id, key }`.

### Planner hook

Private `fetch_org_memory_block(mission_id, title)` runs before planning:
1. Tokenize title into keywords via `keyword_extract`.
2. `search(keywords, 5)` — top-5 relevant memories.
3. Format as a markdown block; publish `OrgMemoryRecalled { mission_id, block_preview }`.

Memory blocks are chained in the planner prompt in this order:
```
<org_memory>   ← most durable, cross-mission
<episodic>     ← past attempts of similar missions
<project>      ← workspace-specific facts
```

Joined with `\n\n---\n\n`. Only sections with content are included.

### Helpers (`crates/forge-mission/src/lib.rs`)

- `keyword_extract(&str) -> Vec<String>` — lowercase, split on non-alphanumeric, drop tokens with `len < 3`, dedup. 3 unit tests.
- `keyify(&str) -> String` — normalized single-line form.

### Events (`crates/forge-domain/src/event.rs`)

- `OrgMemoryLearned { mission_id, memory_id, key }`
- `OrgMemoryRecalled { mission_id, block_preview }`

Both fire under `AggregateKind::Mission`.

### IPC + UI

- Commands: `list_org_memory(limit=200)`, `delete_org_memory(id)`.
- `Settings > Organizational Memory` section:
  - Rows show `#id`, timestamp, tag badges (max 4), key (bold), full value.
  - "Retire" button on each row (confirm dialog; soft-delete).
- Frontend event union + summaries updated for both new events.

### Verify

```powershell
cargo run -p forge-runtime --example org_memory_smoke     # 4/4 scenarios pass
```

Covers insert × 3, list_active ordering, search by tag (rust=2, python=1), and idempotent retire.


## Phase 5 — HTTP API server (SHIPPED)

**Motivation (agent.txt):** *"Any tool that speaks HTTP should be able to drive Forge — CLIs, IDE plugins, chatbots."* Phase 5 exposes the same Runtime the desktop UI uses over a loopback HTTP server, plus a non-streaming OpenAI Chat-Completions compat shim so existing OpenAI-SDK code works unmodified.

### Endpoints

| Method | Path                          | Notes                                        |
|--------|-------------------------------|----------------------------------------------|
| GET    | `/health`                     | Unauthenticated liveness probe.              |
| POST   | `/missions`                   | `{title, description, plan_only?}` → `{id}`. `plan_and_run` is dispatched **out-of-band** — subscribe to `/events` for progress. |
| GET    | `/missions`                   | List all mission summaries.                  |
| GET    | `/missions/:id`               | Full detail: mission + goals + tasks_by_goal.|
| POST   | `/missions/:id/cancel`        | Idempotent cancel → 202.                     |
| POST   | `/missions/:id/extend`        | `{prompt}` — append a follow-up.             |
| GET    | `/events?since=<seq>&mission=<uuid>` | SSE stream of `EventEnvelope`. `since` skips replay; `mission` filters to a single mission's cascade. |
| POST   | `/v1/chat/completions`        | OpenAI-compat shim. **Non-streaming only.** Maps `messages[]` → `(title, description)`, runs mission to termination (5 min cap), returns choices + finish_reason. |

### Auth

Every route except `/health` requires `Authorization: Bearer <token>`. Token is read once at boot from the env var named by `RuntimeConfig.api_token_env` (default `FORGE_API_TOKEN`); empty or unset = auth disabled with a boot-time WARN. Comparison uses `subtle`-style constant-time equality (4 unit tests).

### Design decisions

1. **Loopback-only default.** `RuntimeConfig::api_bind` defaults to `None`. Desktop sets it to `Some(127.0.0.1:7823)`. Plain HTTP is only safe on 127.0.0.1 — TLS + RBAC are out of scope for this phase.
2. **Raw-UUID over the wire, always.** `MissionId::Display` renders `msn_<uuid>` but `#[serde(transparent)]` serializes raw — the exact bug that broke the desktop mission-filter in Phase 4. Every response body uses `id.as_uuid().to_string()`.
3. **`POST /missions` is fire-and-forget.** The handler creates the mission, spawns `plan_and_run` in a detached tokio task, and returns the id immediately. A blocking implementation would hold the HTTP connection for the entire mission — unacceptable for long-running work.
4. **SSE via `BroadcastStream`.** The events subscription is a `tokio::sync::broadcast::Receiver<EventEnvelope>` wrapped by `tokio_stream::wrappers::BroadcastStream`. Lagged frames are dropped silently; the next in-order envelope wins.
5. **Mission filter is defensive-only.** Server-side we don't have goal→mission / task→goal indices, so only direct-mission events are strict-matched; task/goal-scoped events flow through and the client (which does have those indices via `useEventsStore`) can drop them.
6. **OpenAI shim is minimal but honest.** `stream: true` returns 400 with "subscribe to /events instead" — implementing SSE-styled `data: {"choices":...}\n\n` frames is a large chunk of work with tiny payoff since `/events` already exists. Function-calling / logprobs / seed / n>1 are all silently ignored.

### Wiring

- `crates/forge-server/` — new crate. `lib.rs` (router + REST handlers + SSE), `openai_compat.rs` (shim). 8 unit tests.
- `crates/forge-runtime/src/lib.rs` — added `api_bind: Option<SocketAddr>` + `api_token_env: String` fields to `RuntimeConfig`; `Runtime::boot` spawns `forge_server::serve(bind, state)` in a tokio task when `api_bind.is_some()`.
- `apps/forge-desktop/src-tauri/src/lib.rs` — desktop sets `api_bind = Some(127.0.0.1:7823)`, exposes `api_status()` IPC.
- `apps/forge-desktop/frontend/src/views/Settings.tsx` — new **HTTP API** section shows bind, token status, endpoint catalog, and a copy-paste PowerShell curl example.

### Verify

```powershell
cargo run -p forge-server --example api_smoke   # 7/7 assertions pass
```

Boots a real Runtime on an ephemeral loopback port, drives it entirely over TCP with `reqwest`: `/health`, wrong-bearer 401, `POST /missions`, `GET /missions/:id`, cancel, `/v1/chat/completions` returning `finish_reason="error"` (dummy LLM key is expected), `/events` SSE stream. LLM-free.

### Curl walkthrough (PowerShell)

```powershell
$env:FORGE_API_TOKEN = "s3cr3t"           # match Settings > HTTP API "token_env"
# Health
curl http://127.0.0.1:7823/health
# Create mission (background execution)
curl.exe -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
  -H "Content-Type: application/json" `
  -d '{"title":"try it","description":"say hi"}' `
  http://127.0.0.1:7823/missions
# Stream events
curl.exe -N -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
  http://127.0.0.1:7823/events
# OpenAI-compat
curl.exe -H "Authorization: Bearer $env:FORGE_API_TOKEN" `
  -H "Content-Type: application/json" `
  -d '{"model":"forge","messages":[{"role":"user","content":"hi"}]}' `
  http://127.0.0.1:7823/v1/chat/completions
```


## Phase 5b — Headless CLI + VS Code extension + Signed skill bundles (SHIPPED)

**Motivation (agent.txt):** *"Any tool that speaks HTTP should be able to drive Forge."* Phase 5a exposed the HTTP surface; Phase 5b delivers the three highest-value clients so users don't have to write curl scripts:

1. `forge` — a headless CLI that wraps every route.
2. A VS Code extension that speaks the OpenAI-compat shim.
3. Signed skill bundles so shared skills can be trusted.

### 1 · `apps/forge-cli` — the `forge` binary

Single-binary CLI built on `clap` + `reqwest` + `eventsource-stream`. Reads `FORGE_API_URL` and `FORGE_API_TOKEN` from the env by default; both are overridable per invocation with `--url` and `--token`.

Subcommands:

| Command | Behaviour |
|---|---|
| `forge health` | Ping `/health`. Exit 0 iff 2xx. Ignores the bearer. |
| `forge missions list [--status …] [--limit N]` | GET `/missions`, print a table. `--json` for machine output. |
| `forge missions get <ID>` | GET `/missions/:id`. Pretty by default, `--json` for JSON. |
| `forge missions create <TITLE> [--description] [--plan-only]` | POST `/missions`. |
| `forge missions cancel <ID>` | POST `/missions/:id/cancel`. |
| `forge missions extend <ID> <PROMPT>` | POST `/missions/:id/extend`. |
| `forge run <TITLE> [--wait] [--stream]` | Shorthand for create+wait. `--stream` tails events during the wait. |
| `forge events [--mission ID] [--since N] [--follow \| --once]` | SSE tail of `/events`. |
| `forge chat <PROMPT> [--system …]` | POST `/v1/chat/completions`, print response body. |
| `forge skill bundle DIR --out FILE --key KEY` | Package a skill directory into a signed `.forgebundle.json`. |
| `forge skill verify FILE [--pubkey KEY]` | Verify signature; optionally assert exact pubkey. |
| `forge skill install FILE [--dest DIR] [--pubkey KEY] [--force]` | Verify then unpack. Refuses path-traversal filenames. |
| `forge keygen --out FILE` | Generate an ed25519 keypair (private → `FILE`, public → `FILE.pub`). |

Design decisions:
- **`--json` global flag** — every subcommand emits either human-friendly output (default) or newline-delimited JSON so the CLI composes with `jq`, PowerShell `ConvertFrom-Json`, etc.
- **Colour-free** — this is a CI-friendly CLI. Where we want to draw attention we use plain-text sigils (`✓ ✗ …`) that render everywhere.
- **`Command::spawn` in tests, not the reqwest client** — the integration test shells out to the *built* binary so we test the real code paths a user sees, not a fake in-process client.
- **`RUST_LOG` gates tracing** — no chatty output by default, opt-in via env var.

### 2 · `apps/forge-vscode` — VS Code extension

TypeScript extension bundled with esbuild. Registers three commands:

- `Forge: Check server health` — pings `/health`, shows a notification.
- `Forge: Run mission (prompt)` — input box for title (+ optional description), POSTs `/missions`, shows the id.
- `Forge: Send selection as chat` — sends the current editor selection (or prompts) to `/v1/chat/completions`, opens a scratch markdown doc with the response.

Settings: `forgeOs.apiUrl`, `forgeOs.apiToken`. `$env:FORGE_API_TOKEN` beats settings.json so secrets don't have to live in plain text. Extension is ~5 KB compiled — deliberately tiny, all state lives in Forge.

### 3 · Signed skill bundles (`apps/forge-cli/src/bundle.rs`)

Bundle shape:

```jsonc
{
  "manifest":  { "name": "…", "version": 1, … },   // parsed YAML frontmatter of the primary .md
  "files":    { "rel/path": "<base64 bytes>", … },  // every file under the skill dir, sorted
  "signature": "<base64 ed25519 signature>",        // over {manifest, files}
  "pubkey":    "<base64 ed25519 public key>"        // pubkey that produced the signature
}
```

**What gets signed** is `serde_json::to_vec({manifest, files})`. Two bundles built from the same directory produce byte-identical signed payloads → signatures are reproducible. Signature is checked with `ed25519_dalek::VerifyingKey::verify`.

Trust model: each bundle carries the pubkey that signed it. `verify --pubkey <FILE>` asserts the bundle was signed by exactly the expected key — catches "valid signature from the wrong signer" attacks. `install` refuses filenames containing `..`, leading `/`, or leading `\` — no path-traversal.

Sample workflow:

```powershell
forge keygen --out $env:USERPROFILE\.forge\alice_ed25519
# publish alice_ed25519.pub

forge skill bundle .\my-skill\ --out my-skill.forgebundle.json --key $env:USERPROFILE\.forge\alice_ed25519
# → my-skill.forgebundle.json shipped to the world

# consumer:
forge skill verify my-skill.forgebundle.json --pubkey alice_ed25519.pub
forge skill install my-skill.forgebundle.json --dest $env:APPDATA\com.sarthak.forgeos\skills\active --pubkey alice_ed25519.pub
```

### Verify

```powershell
# unit + integration + tamper detection
cargo test -p forge-cli --test end_to_end -- --nocapture
# expect: 2 passed; 0 failed
#   cli_end_to_end     — spins a live Runtime + API server, drives every route
#   cli_bundle_roundtrip — keygen → bundle → verify → install → tamper → verify fails
```

The `cli_end_to_end` test uses `#[tokio::test(flavor = "multi_thread")]` — a single-threaded runtime starves axum when the test blocks on `Command::output`.

### Files landed

- `apps/forge-cli/Cargo.toml` — new binary crate.
- `apps/forge-cli/src/{main.rs, client.rs, render.rs, bundle.rs}`.
- `apps/forge-cli/src/cmd/{mod.rs, health.rs, missions.rs, events.rs, chat.rs, skill.rs}`.
- `apps/forge-cli/tests/end_to_end.rs` — 2 tests, both green.
- `apps/forge-vscode/{package.json, tsconfig.json, esbuild.mjs, README.md, .gitignore}`.
- `apps/forge-vscode/src/extension.ts` — extension entrypoint.
- `Cargo.toml` root — added `clap`, `ed25519-dalek`, `rand_core`, `base64`, `eventsource-stream`, `directories`, `walkdir` to `[workspace.dependencies]`; added `apps/forge-cli` to `[workspace] members`.


## Bug fix — Mission-filter ID mismatch

`MissionId`'s `Display` renders `msn_<uuid>` but `#[serde(transparent)]` serializes it as a raw UUID. Prior `create_mission` returned `id.to_string()` (prefixed), while every event payload + `list_missions` row uses the raw UUID form via serde. Result: the UI auto-selected the newly-created mission with the prefixed form but every event filtered on it never matched → the mission's DAG loaded but its event timeline stayed empty.

Fixed in `apps/forge-desktop/src-tauri/src/lib.rs` — `create_mission` now returns `id.as_uuid().to_string()` with a comment explaining the trap.

---


## Phase 5c/5d/5e/5f — Streaming shim + RBAC + Gateway + Plugin bundles (SHIPPED)

Wrapped up the Phase-5 ecosystem tier in one push. Four independent shippables, all backed by tests.

### 5c — OpenAI streaming shim

`POST /v1/chat/completions` now honours `"stream": true`. Response is an `Sse<...>` of `chat.completion.chunk` frames matching the OpenAI wire format so `openai` / LangChain clients work unchanged.

- New `streaming_completion(state, mid, model)` in `crates/forge-server/src/openai_compat.rs` — spawns a producer task, subscribes to the event bus via `BroadcastStream`, filters by mission id, and forwards planning / status / replan / skill-selected events as deltas.
- `tokio::select!` between the broadcast stream and a 500 ms status poll so terminal-state detection still happens during quiet periods. 300 s hard timeout → `finish_reason: "length"`.
- Frame order: initial `role: assistant` chunk → content chunks → final chunk with `finish_reason` → `data: [DONE]` sentinel.

### 5d — RBAC (two-role token split)

Two token classes so IDEs / dashboards can be granted read-only access without exposing mission creation.

- `FORGE_API_TOKEN` — Full role. All GETs + all mutations.
- `FORGE_API_READONLY_TOKENS` — comma-separated list. GET-only. Mutating routes return **403 Forbidden**.
- New `Role { Full, ReadOnly }` + `require_full(...)` helper in `crates/forge-server/src/lib.rs`.
- Wired: `create_mission`, `cancel_mission`, `extend_mission`, `chat_completions` all require full. `list_missions`, `get_mission`, `events_sse` accept either role.
- `Runtime::boot` reads the env, calls `ApiState::with_read_only_tokens(...)`, and logs an INFO count when any RO tokens are present.

### 5e — Messaging gateway (`forge-gateway` binary)

New crate at `apps/forge-gateway/`. Axum bridge that speaks to Forge via the OpenAI shim and exposes:

- `GET  /health` — trivial health check.
- `POST /webhook` — generic bearer-guarded intake (`Authorization: Bearer $GATEWAY_SHARED_SECRET`).
- `POST /slack/commands` — Slack slash-command receiver. Verifies `X-Slack-Signature` (HMAC-SHA256 over `v0:{ts}:{body}` with the signing secret; 5-minute replay window; constant-time compare). Acks ephemerally within Slack's 3 s deadline, then a background task calls Forge and POSTs the answer back to `response_url`.

4 unit tests: signature verify OK / tampered body rejected / stale timestamp rejected / router builds. Discord + Telegram would slot in as ~50-line siblings of `slack_slash`.

### 5f — Signed MCP plugin bundles

Same signing pipeline as Phase-5b skill bundles, extended to carry an explicit `kind`:

- New `enum BundleKind { Skill, Plugin }`. Serialized snake_case; both `Bundle.kind` and the internal `Signed.kind` are `Option<BundleKind>` with `skip_serializing_if = "Option::is_none"` — so **pre-5f skill bundles verify byte-identically** (a plain-skill signature does not include a `kind` field).
- Plugin dirs must contain a top-level `mcp.yaml` OR `plugin.yaml` manifest; enforced by new `collect_plugin_files(dir)`.
- New top-level CLI subcommand `forge plugin bundle|verify|install`, sharing dispatch code with `forge skill` via `dispatch_kind(kind, json, op)`. `verify` prints e.g. `signature OK  (kind: Plugin)`.

### Files changed

- `crates/forge-server/src/lib.rs` — `Role`, `ApiConfig.read_only_tokens`, `ApiState::with_read_only_tokens`, `require_full`, `Forbidden(String) → 403`. `check_auth` returns `Result<Role, ApiError>` and does constant-time compares.
- `crates/forge-server/src/openai_compat.rs` — streaming branch in `chat_completions` (return type is now `Result<Response, ApiError>`), plus `streaming_completion` / `chunk` / `sse_data` / `event_to_delta` helpers.
- `crates/forge-runtime/src/lib.rs` — module-const `READONLY_TOKENS_ENV = "FORGE_API_READONLY_TOKENS"`; `Runtime::boot` reads + splits + calls `ApiState::with_read_only_tokens`. Kept a hard-coded env var name to avoid touching the 16+ `RuntimeConfig { ... }` struct-literal sites elsewhere.
- `apps/forge-cli/src/bundle.rs` — `BundleKind`, `Signed.kind`, `sign_bundle(..., BundleKind)`, `collect_plugin_files(...)`.
- `apps/forge-cli/src/cmd/skill.rs` — `dispatch_kind(kind, json, op)` shared entry point.
- `apps/forge-cli/src/main.rs` — `Cmd::Plugin { op: cmd::skill::Op }` variant.
- `apps/forge-gateway/{Cargo.toml, src/main.rs}` — new crate; `hmac 0.12` + `serde_urlencoded 0.7` as direct deps.
- `Cargo.toml` — added `apps/forge-gateway` to `[workspace] members`.
- `apps/forge-cli/tests/end_to_end.rs` — added `cli_plugin_bundle_roundtrip`, `cli_readonly_token_restricts_writes`, `openai_streaming_shim_emits_chunks`. Full suite is now 5 tests, all green.
- `scripts/install-{windows.ps1, macos.sh}` — added optional step 6 for `cargo install --path apps/forge-gateway`.

### Verification

```
cargo test -p forge-cli --test end_to_end
    test result: ok. 5 passed; 0 failed
cargo test -p forge-gateway
    test result: ok. 4 passed; 0 failed
cargo check --workspace --tests --examples
    Finished — no warnings, no errors
```

---




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
