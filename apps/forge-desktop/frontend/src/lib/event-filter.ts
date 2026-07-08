import type { ForgeEvent, EventEnvelope } from "@/lib/events";

/**
 * Resolve the mission_id an event belongs to, using the provided
 * goal→mission and task→goal indices for derived cases.
 * Returns null when the event is truly global (e.g. MCP server lifecycle).
 */
export function eventMissionId(
  ev: ForgeEvent,
  goalToMission: Record<string, string>,
  taskToGoal: Record<string, string>,
): string | null {
  switch (ev.type) {
    // Direct mission id
    case "mission_created":
    case "mission_planning_started":
    case "mission_planning_completed":
    case "mission_planning_failed":
    case "mission_status_changed":
    case "mission_cancel_requested":
      return ev.id;
    case "skills_selected":
    case "replan_requested":
    case "plan_revised":
    case "replan_cap_exceeded":
    case "mission_reflection_completed":
    case "skill_proposal_written":
    case "mission_cost_summary":
    case "episodic_recall_surfaced":
      return ev.mission_id;
    case "checkpoint_created":
    case "checkpoint_skipped":
      return ev.mission_id ?? null;
    // Goal-scoped
    case "goal_created":
      return ev.mission_id;
    case "goal_status_changed":
      return goalToMission[ev.id] ?? null;
    // Task-scoped (task -> goal -> mission)
    case "task_created": {
      const goalId = ev.goal_id;
      return goalToMission[goalId] ?? null;
    }
    case "task_started":
    case "task_completed":
    case "task_failed": {
      const goalId = taskToGoal[ev.id];
      return goalId ? goalToMission[goalId] ?? null : null;
    }
    case "task_input_refreshed":
    case "tool_invoked":
    case "policy_denied":
    case "policy_approval_requested":
    case "policy_approval_granted": {
      const goalId = taskToGoal[ev.task_id];
      return goalId ? goalToMission[goalId] ?? null : null;
    }
    case "mcp_tool_invoked": {
      if (!ev.task_id) return null;
      const goalId = taskToGoal[ev.task_id];
      return goalId ? goalToMission[goalId] ?? null : null;
    }
    // LLM (has optional mission_id)
    case "llm_requested":
    case "llm_responded":
    case "llm_failed":
      return ev.mission_id ?? null;
    // Global
    case "mcp_server_started":
    case "mcp_server_failed":
      return null;
    default:
      return null;
  }
}

export type EventCategory = "mission" | "goal" | "task" | "llm" | "plugin" | "meta";

export function eventCategory(ev: ForgeEvent): EventCategory {
  const t = ev.type;
  if (t.startsWith("mission_cost") || t.startsWith("mission_reflection") || t.startsWith("skill_proposal") || t.startsWith("skills_selected") || t.startsWith("replan_") || t === "plan_revised" || t === "episodic_recall_surfaced" || t === "checkpoint_created" || t === "checkpoint_skipped") return "meta";
  if (t.startsWith("mission_")) return "mission";
  if (t.startsWith("goal_")) return "goal";
  if (t.startsWith("task_") || t === "tool_invoked" || t.startsWith("policy_")) return "task";
  if (t.startsWith("llm_")) return "llm";
  if (t.startsWith("mcp_")) return "plugin";
  return "meta";
}

export function filterEvents(
  events: EventEnvelope[],
  {
    missionId,
    categories,
    query,
    goalToMission,
    taskToGoal,
  }: {
    missionId: string | null;
    categories: Set<EventCategory>;
    query: string;
    goalToMission: Record<string, string>;
    taskToGoal: Record<string, string>;
  },
): EventEnvelope[] {
  const q = query.trim().toLowerCase();
  return events.filter((env) => {
    if (!categories.has(eventCategory(env.event))) return false;
    if (missionId) {
      const id = eventMissionId(env.event, goalToMission, taskToGoal);
      if (id !== missionId) return false;
    }
    if (q) {
      if (!JSON.stringify(env.event).toLowerCase().includes(q)) return false;
    }
    return true;
  });
}
