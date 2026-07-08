/**
 * Thin, typed wrappers around Tauri v2 IPC. Keeps invocation strings + payload
 * shapes in one place so refactors don't slip through in a hundred views.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EventEnvelope, MissionDetail, MissionSummary } from "./events";

export async function createMission(title: string, description: string): Promise<string> {
  const res = await invoke<{ id: string }>("create_mission", { title, description });
  return res.id;
}

export async function planAndRun(missionId: string): Promise<void> {
  await invoke("plan_and_run", { missionId });
}

export async function cancelMission(missionId: string): Promise<void> {
  await invoke("cancel_mission", { missionId });
}

export async function extendMission(missionId: string, prompt: string): Promise<void> {
  await invoke("extend_mission", { missionId, prompt });
}

export async function approveTask(taskId: string): Promise<void> {
  await invoke("approve_task", { taskId });
}

export async function listMissions(): Promise<MissionSummary[]> {
  return invoke<MissionSummary[]>("list_missions");
}

export async function getMission(missionId: string): Promise<MissionDetail> {
  return invoke<MissionDetail>("get_mission", { missionId });
}

export async function replayEvents(since?: number): Promise<EventEnvelope[]> {
  return invoke<EventEnvelope[]>("replay_events", { since: since ?? null });
}

/** Poll-based readiness probe. Succeeds once `AppState` is managed by Tauri.
 *  Use to bootstrap the "ready" flag even if the `forge://runtime-ready`
 *  broadcast event fired before the frontend attached its listener. */
export async function runtimeStatus(): Promise<void> {
  await invoke("runtime_status");
}

/** Subscribe to the live event stream. Returns an unlisten fn. */
export async function subscribeEvents(cb: (env: EventEnvelope) => void): Promise<UnlistenFn> {
  return listen<EventEnvelope>("forge://event", (msg) => cb(msg.payload));
}

/** Subscribe to one-shot runtime lifecycle events. */
export async function subscribeRuntimeReady(cb: () => void): Promise<UnlistenFn> {
  return listen<null>("forge://runtime-ready", () => cb());
}

export async function subscribeRuntimeError(cb: (err: string) => void): Promise<UnlistenFn> {
  return listen<string>("forge://runtime-error", (msg) => cb(msg.payload));
}

// ---- Phase 3: checkpoints, secrets, audit export ----

export interface Checkpoint {
  sha: string;
  short_sha: string;
  subject: string;
  timestamp: string;
  mission_id: string | null;
  task_id: string | null;
  tool: string | null;
  files_changed: number;
  insertions: number;
  deletions: number;
}

export interface SecretStatus {
  name: string;
  set: boolean;
  source: "env" | "keyring" | "unset";
}

export interface AuditExportResult {
  path: string;
  missions: number;
  goals: number;
  tasks: number;
  events: number;
  reflections: number;
}

export async function listCheckpoints(missionId?: string, limit?: number): Promise<Checkpoint[]> {
  return invoke<Checkpoint[]>("list_checkpoints", {
    missionId: missionId ?? null,
    limit: limit ?? null,
  });
}

export async function revertCheckpoint(sha: string): Promise<void> {
  await invoke("revert_checkpoint", { sha });
}

export async function listSecretStatus(): Promise<SecretStatus[]> {
  return invoke<SecretStatus[]>("list_secret_status");
}

export async function setSecret(name: string, value: string): Promise<void> {
  await invoke("set_secret", { name, value });
}

export async function deleteSecret(name: string): Promise<void> {
  await invoke("delete_secret", { name });
}

export async function exportAudit(dest: string): Promise<AuditExportResult> {
  return invoke<AuditExportResult>("export_audit", { dest });
}

// ---- Phase 4a/4b: skills governance ----

export interface SkillProposalSummary {
  name: string;
  version: string;
  description: string;
  filename: string;
}

export interface SkillVersion {
  name: string;
  sha: string;
  version: string;
  origin: "proposal" | "handcrafted" | "rollback" | "curated";
  origin_mission_id: string | null;
  parent_sha: string | null;
  promoted_at: string;
  retired_at: string | null;
  reason: string | null;
}

export interface CuratorSuggestion {
  name: string;
  kind: "duplicate" | "unused" | "merge_candidate";
  evidence: string;
}

export interface CuratorReport {
  suggestions: CuratorSuggestion[];
  auto_archived: Array<[string, string]>;
  merge_proposals: string[];
}

export interface ValidationCheck {
  id: string;
  severity: "hard" | "soft";
  passed: boolean;
  message: string;
}

export interface ValidationReport {
  ok: boolean;
  checks: ValidationCheck[];
}

export async function listSkillProposals(): Promise<SkillProposalSummary[]> {
  return invoke<SkillProposalSummary[]>("list_skill_proposals");
}
export async function approveSkillProposal(filename: string): Promise<string> {
  return invoke<string>("approve_skill_proposal", { filename });
}
export async function rejectSkillProposal(filename: string): Promise<void> {
  await invoke("reject_skill_proposal", { filename });
}
export async function validateSkillProposal(filename: string): Promise<ValidationReport> {
  return invoke<ValidationReport>("validate_skill_proposal", { filename });
}

export async function listActiveSkills(): Promise<SkillVersion[]> {
  return invoke<SkillVersion[]>("list_active_skills");
}
export async function listSkillVersions(name: string): Promise<SkillVersion[]> {
  return invoke<SkillVersion[]>("list_skill_versions", { name });
}
export async function rollbackSkill(name: string, sha: string, reason?: string): Promise<SkillVersion> {
  return invoke<SkillVersion>("rollback_skill", { name, sha, reason: reason ?? null });
}
export async function retireSkill(name: string, reason: string): Promise<string | null> {
  return invoke<string | null>("retire_skill", { name, reason });
}
export async function runCurator(): Promise<CuratorSuggestion[]> {
  return invoke<CuratorSuggestion[]>("run_curator");
}

export async function curatorScan(apply: boolean): Promise<CuratorReport> {
  return invoke<CuratorReport>("curator_scan", { apply });
}

// ---- Phase 4d/4f: mission queue + org memory ----

export interface MissionQueueRow {
  id: number;
  mission_id: string;
  status: "queued" | "claimed" | "done" | "failed";
  claimed_by: string | null;
  claimed_at: string | null;
  heartbeat_at: string | null;
  finished_at: string | null;
  error: string | null;
  enqueued_at: string;
}

export interface QueueStatus {
  workers: number;
  queued: number;
  claimed: number;
  recent: MissionQueueRow[];
}

export interface OrgMemoryRow {
  id: number;
  key: string;
  value: string;
  tags: string[];
  source_mission_id: string | null;
  created_at: string;
  retired_at: string | null;
}

export async function queueStatus(): Promise<QueueStatus> {
  return invoke<QueueStatus>("queue_status");
}

export async function listOrgMemory(limit?: number): Promise<OrgMemoryRow[]> {
  return invoke<OrgMemoryRow[]>("list_org_memory", { limit: limit ?? null });
}

export async function deleteOrgMemory(id: number): Promise<boolean> {
  return invoke<boolean>("delete_org_memory", { id });
}

// ---------- Phase 5 — HTTP API server ----------

export interface ApiStatus {
  /** e.g. "127.0.0.1:7823" or null when server disabled. */
  bind: string | null;
  /** Env var name we read the bearer token from. */
  token_env: string;
  /** True if that env var was non-empty at boot. */
  token_set: boolean;
  /** Copy-paste PowerShell curl example. */
  curl_example: string;
  /** [method, path] pairs (documentation only). */
  endpoints: [string, string][];
}

export async function apiStatus(): Promise<ApiStatus> {
  return invoke<ApiStatus>("api_status");
}
