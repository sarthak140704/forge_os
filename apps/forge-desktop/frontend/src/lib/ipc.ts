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
