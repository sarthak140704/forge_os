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
- **Memory layers**: Working / Project (`AGENTS.md`, `.forge.md`) ✓ / User (`user.md`, personal preferences) ✓ / Episodic (keyword recall of prior terminal missions) ✓ / Semantic (pluggable embedding provider + cosine over `org_memory`) ✓ landed in Phase 6a.
- **Cost / usage tracking**: per-mission LLM call/token/latency accumulation with `MissionCostSummary` events + live `LlmRequested`/`LlmResponded`/`LlmFailed` streaming through the event bus. ✓ landed
- **Feature flags**: typed `feature-flags.toml` under app-data with env overrides (`FORGE_FLAG_*`). ✓ landed
- **Cancellation IPC**: `cancel_mission` Tauri command emits `MissionCancelRequested` event before flipping the cooperative token. ✓ landed
- **Skill promotion flow**: `list_skill_proposals`/`approve_skill_proposal`/`reject_skill_proposal` IPC commands moving files between `proposed/` and `active/`. ✓ landed
- **OpenTelemetry** exporter (traces, metrics, logs). ✓ landed in Phase 6b (OTLP HTTP-protobuf, opt-in via `FORGE_OTLP_ENDPOINT`).
- **Cross-platform** builds: macOS + Linux CI. ✓ landed (Phase 7 — `.github/workflows/ci.yml`: Rust build+test matrix on ubuntu/macos/windows, frontend build, and a Linux Tauri `cargo check` job with WebView/GTK system deps; fmt+clippy run informationally).
- **Provider expansion**: Anthropic ✓ (Phase 6c), OpenAI ✓, Gemini ✓ (Phase 6c), Azure OpenAI ✓, LM Studio ✓, vLLM ✓ (Phase 6g).

## Phase 3 — Governance & Safety
- **Plugin sandbox**: Wasmtime for skill code; child-process isolation for MCP servers.
- **Approval workflows**: multi-step, quorum, delegated approvers. (basic single-step PolicyApprovalRequested/Granted already exists)
- **Shadow-git checkpoints**: every filesystem mutation goes into a shadow repo → 1-click revert. ✓ landed (auto-snapshot after every mutating tool, `list_checkpoints`/`revert_checkpoint` IPC, revert UI in Settings)
- **Secret store**: OS keyring integration (Windows Credential Manager, macOS Keychain, libsecret). ✓ landed (`keyring` crate v3, env vars still win, `list_secret_status`/`set_secret`/`delete_secret` IPC, Settings UI)
- **Audit export**: SOC 2-style report generation from event store. ✓ landed (JSON bundle of missions/goals/tasks/events/reflections via `export_audit` IPC + file picker)

## Phase 4 — Learning & Scale
- **Learning Engine**: evaluate → reflect → extract → version → validate → promote pipeline. ✓ landed (reflect_and_learn + AutoPromoter + Curator across 4a-4c)
- **Skill versioning**: content-addressed skills, monotonic version numbers, rollback. ✓ landed (Phase 4a)
- **Distributed execution**: multi-worker pool, Postgres-backed queue, leader election. ✓ landed as *in-process* worker pool + persisted SQLite queue with crash recovery (Phase 4d). Networked/leader-election is Phase 5.
- **PostgreSQL backend**: swap SQLite via repository trait, no domain changes. ✓ landed *swap boundary* (`PersistenceHandles::open(url)` with honest `NotYetImplemented` stub; real PG impl is Phase 5) (Phase 4e)
- **Organizational Memory**: Honcho integration or equivalent dialectic memory. ✓ landed (Phase 4f — reflection insights promoted to durable memory rows, LIKE-recall injected as a third planner memory block; embeddings/semantic recall is Phase 5)
- **Curator**: automated skill deprecation, dedupe, merge. ✓ landed (Phase 4c)

## Phase 5 — Ecosystem
- **Marketplace**: signed skill & plugin bundles. ✓ landed *signed skill bundles* (Phase 5b — `forge skill bundle|verify|install` with ed25519 signatures + `forge keygen`) and *signed plugin bundles* (Phase 5f — `forge plugin bundle|verify|install`, accepts `mcp.yaml`/`plugin.yaml` manifests, kind stored in the signed payload). Distribution registry / discovery UX still deferred.
- **Team edition**: multi-user missions, RBAC, org-level policies. ✓ landed *RBAC v1* (Phase 5d — two-role split: Full tokens (env `FORGE_API_TOKEN`) can do everything; ReadOnly tokens (comma-separated `FORGE_API_READONLY_TOKENS`) can only GET). Multi-user missions + org-level policies still deferred.
- **API server**: OpenAI-compatible endpoint (like Hermes) so any frontend can drive Forge. ✓ landed as loopback HTTP + bearer + SSE + OpenAI-compat shim (Phase 5a) with streaming `chat.completion.chunk` frames (Phase 5c). TLS + per-user auth + WebSocket transport are deferred.
- **Headless CLI**: `forge` binary wrapping the API. ✓ landed (Phase 5b — all subcommands + signed skill bundles + integration test).
- **Messaging gateway**: reuse or wrap Hermes gateway (Telegram/Slack/etc.). ✓ landed (Phase 5e + Phase 6e — `forge-gateway` binary; Slack slash-command receiver with HMAC-verified signatures, generic `POST /webhook`, Discord `/interactions` with ed25519 verification + deferred-ack pattern, Telegram `/webhook` with optional secret token.)
- **ACP editor integration**: VS Code, Zed, JetBrains. ✓ landed *VS Code extension* (Phase 5b — health / run mission / send-selection-as-chat via OpenAI shim). Zed / JetBrains still open.
- **Voice mode, image gen, TTS, browser automation** (all deferred from spec §Media & Web).

## Phase 6 — Depth (shipped this session)
- **6a Semantic memory**: pluggable embedding provider (OpenAI, Ollama), cosine-ranked recall over `org_memory`, lazy backfill on new writes. Falls back to keyword LIKE search when no provider is configured or the semantic hit list is empty. ✓ landed
- **6b OpenTelemetry OTLP exporter**: opt-in via `FORGE_OTLP_ENDPOINT`, service name via `FORGE_OTEL_SERVICE_NAME`, HTTP-protobuf on port 4318, tokio batch runtime. Zero-cost when the env var is unset (fmt-only subscriber). Never fails boot. ✓ landed
- **6c Anthropic + Gemini providers**: hoisted-system messages for Claude, camelCase-normalized generateContent for Gemini, prompt-based JSON mode for Anthropic, `responseMimeType` for Gemini. Auto-registered in the failover chain when keys are present. ✓ landed
- **6e Gateway adapters — Discord + Telegram**: `/discord/interactions` with ed25519 signature verification, PING→PONG, deferred ack + async PATCH-to-webhook for slash commands; `/telegram/webhook` with optional `X-Telegram-Bot-Api-Secret-Token` check, `sendMessage` reply. ✓ landed
- **6g Provider expansion — Azure OpenAI + LM Studio + vLLM**: dedicated `AzureOpenAiProvider` (deployment-in-URL, `api-key` header, `api-version` query param, default `2024-06-01`); LM Studio and vLLM reuse the OpenAI-compatible adapter via `OpenAiProvider::with_name(...)`. Azure registers only when key + endpoint + deployment are all present; local backends are opt-in via `LMSTUDIO_BASE_URL` / `VLLM_BASE_URL`. ✓ landed
- **6d Wasmtime skill sandbox** — deferred (needs its own project cadence).
- **6f Marketplace registry** — deferred (distribution + discovery UX). Signed bundles + `install-from-url` still ship in 5b/5f.

## Explicit non-goals for a *personal* project
- Kubernetes-native orchestration
- HA control plane
- Anything requiring paid infra beyond a single dev's LLM API key

## Phase 7 — Breadth (shipped this session)
- **Skill library expansion**: grew the seeded skill catalogue from 4 → **20**
  playbooks under `config/skills/active/`, covering agent.txt's Skill System +
  Plugin catalogue: `docker`, `kubernetes`, `terraform`, `github-cli`, `aws`,
  `postgres`, `redis`, `go-module`, `react-app`, `security-review`,
  `code-review`, `documentation`, `refactoring`, `database-migration`,
  `incident-response`, `release-management` (plus the original `rust-crate`,
  `node-project`, `python-project`, `git-repo`). Each is a safety-first
  playbook that orchestrates the domain's real CLIs through the resolvable
  built-in tools (`fs.*`, `code.search`, `shell.run`) + `mcp:` tools. All are
  embedded into the desktop first-run bootstrap (`SEED_SKILLS`) and gated by a
  new portable regression test `crates/forge-skills/tests/seed_skills.rs`
  (validates every seed skill passes the hard checks, names are unique, count
  is stable). ✓ landed
- **Architecture diagrams**: `docs/DIAGRAMS.md` — Mermaid diagrams closing
  agent.txt *Deliverables* 21–27: crate dependency graph, mission + goal
  lifecycle state machines, plan→execute→reflect sequence, event-sourcing data
  flow, skill lifecycle, and plugin/MCP lifecycle. Grounded in the real crate
  graph and event/state enums. ✓ landed
- **Cross-platform CI**: see Phase 2 entry above. ✓ landed

### Still deferred (need their own cadence or external infra)
- Wasmtime skill sandbox (6d); real PostgreSQL impl (needs a live DB to verify);
  marketplace **registry**/discovery UX (6f — signed bundles already ship);
  API TLS + per-user auth + WebSocket; multi-user team missions + org policies;
  Zed / JetBrains editors; voice / image / TTS / browser automation.
