# Forge OS — Roadmap

## Phase 1 — Vertical Slice (this session's target)
See `IDEATION.md §4`. Goal: a Tauri app that boots, accepts a mission, plans it against
a real LLM (OpenRouter or local Ollama), executes with policy enforcement, and streams
events into a live React Flow DAG viewer.

**Definition of done:**
- `cargo check --workspace` green
- `cargo test --workspace` green
- `cd apps/forge-desktop/frontend && npm run build` green
- `cargo tauri dev` boots the app on Windows
- Manual: creating a "list files in workspace and summarise" mission produces a DAG
  and completes with visible events.

## Phase 2 — Extensibility
- **Skill Runtime v2**: load `SKILL.md` files (agentskills.io format); skills compose tools. ✓ landed
- **MCP client**: `forge-mcp` crate; each MCP server becomes a plugin surfacing tools. ✓ landed
- **Just-in-time task materialization**: the initial planner emits placeholder task inputs (e.g. `"[insert directories here]"`) for tasks that depend on upstream results that haven't run yet. At execution time, before each dependent goal runs, its tasks' inputs are re-materialized by the LLM using the completed upstream results as context. Observable via `TaskInputRefreshed` events. ✓ landed
- **Memory layers**: Working / Project (`AGENTS.md`, `.forge.md`) ✓ / User (`user.md`, personal preferences) ✓ / Episodic (keyword recall of prior terminal missions) ✓ / Semantic (sqlite-vss, embeddings) — deferred.
- **Cost / usage tracking**: per-mission LLM call/token/latency accumulation with `MissionCostSummary` events + live `LlmRequested`/`LlmResponded`/`LlmFailed` streaming through the event bus. ✓ landed
- **Feature flags**: typed `feature-flags.toml` under app-data with env overrides (`FORGE_FLAG_*`). ✓ landed
- **Cancellation IPC**: `cancel_mission` Tauri command emits `MissionCancelRequested` event before flipping the cooperative token. ✓ landed
- **Skill promotion flow**: `list_skill_proposals`/`approve_skill_proposal`/`reject_skill_proposal` IPC commands moving files between `proposed/` and `active/`. ✓ landed
- **OpenTelemetry** exporter (traces, metrics, logs).
- **Cross-platform** builds: macOS + Linux CI.
- **Provider expansion**: Anthropic, OpenAI ✓, Gemini, Azure OpenAI, LM Studio, vLLM adapters.

## Phase 3 — Governance & Safety
- **Plugin sandbox**: Wasmtime for skill code; child-process isolation for MCP servers.
- **Approval workflows**: multi-step, quorum, delegated approvers. (basic single-step PolicyApprovalRequested/Granted already exists)
- **Shadow-git checkpoints**: every filesystem mutation goes into a shadow repo → 1-click revert. ✓ landed (auto-snapshot after every mutating tool, `list_checkpoints`/`revert_checkpoint` IPC, revert UI in Settings)
- **Secret store**: OS keyring integration (Windows Credential Manager, macOS Keychain, libsecret). ✓ landed (`keyring` crate v3, env vars still win, `list_secret_status`/`set_secret`/`delete_secret` IPC, Settings UI)
- **Audit export**: SOC 2-style report generation from event store. ✓ landed (JSON bundle of missions/goals/tasks/events/reflections via `export_audit` IPC + file picker)

## Phase 4 — Learning & Scale
- **Learning Engine**: evaluate → reflect → extract → version → validate → promote pipeline.
- **Skill versioning**: content-addressed skills, monotonic version numbers, rollback.
- **Distributed execution**: multi-worker pool, Postgres-backed queue, leader election.
- **PostgreSQL backend**: swap SQLite via repository trait, no domain changes.
- **Organizational Memory**: Honcho integration or equivalent dialectic memory.
- **Curator**: automated skill deprecation, dedupe, merge.

## Phase 5 — Ecosystem
- **Marketplace**: signed skill & plugin bundles.
- **Team edition**: multi-user missions, RBAC, org-level policies.
- **API server**: OpenAI-compatible endpoint (like Hermes) so any frontend can drive Forge.
- **Messaging gateway**: reuse or wrap Hermes gateway (Telegram/Slack/etc.).
- **ACP editor integration**: VS Code, Zed, JetBrains.
- **Voice mode, image gen, TTS, browser automation** (all deferred from spec §Media & Web).

## Explicit non-goals for a *personal* project
- Kubernetes-native orchestration
- HA control plane
- Anything requiring paid infra beyond a single dev's LLM API key
