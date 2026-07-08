/**
 * TypeScript mirror of the Rust event & aggregate types.
 * Kept manually in sync with forge-domain until Phase 2 introduces
 * ts-rs / typeshare codegen.
 */

export type MissionStatus =
  | "draft" | "planning" | "ready" | "running" | "paused"
  | "completed" | "failed" | "cancelled";

export type GoalStatus =
  | "pending" | "ready" | "running" | "completed" | "failed" | "skipped";

export type TaskStatus =
  | "pending" | "pending_approval" | "running" | "completed"
  | "failed" | "cancelled" | "denied";

export interface MissionSummary {
  id: string;
  title: string;
  status: MissionStatus;
  created_at: string;
  goal_count: number;
  completed_goal_count: number;
}

export interface Mission {
  id: string;
  title: string;
  description: string;
  status: MissionStatus;
  created_at: string;
  updated_at: string;
  goals: string[];
}

export interface Goal {
  id: string;
  mission_id: string;
  title: string;
  description: string;
  status: GoalStatus;
  depends_on: string[];
  confidence: number;
  priority: number;
  retries_remaining: number;
  tasks: string[];
}

export interface Task {
  id: string;
  goal_id: string;
  tool: string;
  input: unknown;
  status: TaskStatus;
  result: unknown | null;
  error: string | null;
  attempts: number;
}

export interface MissionDetail {
  mission: Mission;
  goals: Goal[];
  tasks_by_goal: Record<string, Task[]>;
}

/** Discriminated union — must match forge-domain::ForgeEvent. */
export type ForgeEvent =
  | { type: "mission_created"; id: string; title: string }
  | { type: "mission_planning_started"; id: string }
  | { type: "mission_planning_completed"; id: string; goal_count: number }
  | { type: "mission_planning_failed"; id: string; error: string }
  | { type: "mission_status_changed"; id: string; from: MissionStatus; to: MissionStatus }
  | { type: "mission_cancel_requested"; id: string }
  | { type: "goal_created"; id: string; mission_id: string; title: string; depends_on: string[] }
  | { type: "goal_status_changed"; id: string; from: GoalStatus; to: GoalStatus }
  | { type: "task_created"; id: string; goal_id: string; tool: string }
  | { type: "task_started"; id: string }
  | { type: "task_completed"; id: string; result_summary: string }
  | { type: "task_failed"; id: string; error: string }
  | { type: "task_input_refreshed"; task_id: string; tool: string }
  | { type: "tool_invoked"; task_id: string; tool: string }
  | { type: "policy_denied"; task_id: string; rule: string; reason: string }
  | { type: "policy_approval_requested"; task_id: string; rule: string; reason: string }
  | { type: "policy_approval_granted"; task_id: string }
  | { type: "llm_requested"; request_id: string; provider: string; model: string; mission_id?: string | null }
  | { type: "llm_responded"; request_id: string; latency_ms: number; prompt_tokens: number; completion_tokens: number; mission_id?: string | null; provider?: string; model?: string }
  | { type: "llm_failed"; request_id: string; provider: string; error: string; mission_id?: string | null }
  | { type: "skills_selected"; mission_id: string; skill_names: string[] }
  | { type: "replan_requested"; mission_id: string; iteration: number }
  | { type: "plan_revised"; mission_id: string; iteration: number; added_goals: number }
  | { type: "replan_cap_exceeded"; mission_id: string; iteration: number }
  | { type: "mission_reflection_completed"; mission_id: string; skill_suggestion_count: number }
  | { type: "skill_proposal_written"; mission_id: string; skill_name: string; path: string }
  | { type: "mission_cost_summary"; mission_id: string; llm_calls: number; prompt_tokens: number; completion_tokens: number; total_latency_ms: number }
  | { type: "episodic_recall_surfaced"; mission_id: string; keywords: string[]; prior_count: number; block_preview: string }
  | { type: "checkpoint_created"; sha: string; short_sha: string; tool: string; mission_id?: string | null; task_id?: string | null; label: string }
  | { type: "checkpoint_skipped"; tool: string; mission_id?: string | null; task_id?: string | null; reason: string }
  | { type: "mcp_server_started"; name: string; tools: string[] }
  | { type: "mcp_server_failed"; name: string; error: string }
  | { type: "mcp_tool_invoked"; server: string; tool: string; task_id?: string | null }
  | { type: "skill_promoted"; name: string; sha: string; version: string; origin: string; parent_sha?: string | null; origin_mission_id?: string | null }
  | { type: "skill_rolled_back"; name: string; from_sha?: string | null; to_sha: string; reason?: string | null }
  | { type: "skill_retired"; name: string; sha: string; reason: string }
  | { type: "skill_curation_suggested"; name: string; kind: string; evidence: string }
  | { type: "skill_validation_passed"; filename: string; name: string; soft_failures: string[] }
  | { type: "skill_validation_failed"; filename: string; name: string; failed_checks: string[] }
  | { type: "skill_auto_promoted"; name: string; sha: string; version: string }
  | { type: "skill_auto_archived"; archived_name: string; archived_sha: string; kept_name: string; similarity: number; rule: string }
  | { type: "skill_merge_proposed"; proposal_filename: string; merged_name: string; source_a: string; source_b: string; body_similarity: number }
  | { type: "mission_queued"; mission_id: string; queue_id: number }
  | { type: "org_memory_recalled"; mission_id: string; block_preview: string }
  | { type: "org_memory_learned"; mission_id: string; memory_id: number; key: string };

export interface EventEnvelope {
  seq: number;
  ts: string;
  event: ForgeEvent;
}
