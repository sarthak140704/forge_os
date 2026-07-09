import { useMemo, useState } from "react";
import { useEventsStore } from "@/stores/events";
import { useUiStore } from "@/stores/ui";
import { Card } from "@/components/ui/primitives";
import { cn } from "@/lib/utils";
import type { ForgeEvent } from "@/lib/events";
import {
  filterEvents,
  eventCategory,
  type EventCategory,
} from "@/lib/event-filter";

function summarize(e: ForgeEvent): string {
  switch (e.type) {
    case "mission_created": return `Mission created: ${e.title}`;
    case "mission_planning_started": return "Planning started";
    case "mission_planning_completed": return `Planning completed (${e.goal_count} goals)`;
    case "mission_planning_failed": return `Planning failed: ${e.error}`;
    case "mission_status_changed": return `Mission ${e.from} → ${e.to}`;
    case "mission_cancel_requested": return "Cancellation requested";
    case "goal_created": return `Goal created: ${e.title}`;
    case "goal_status_changed": return `Goal ${e.from} → ${e.to}`;
    case "task_created": return `Task ${e.tool} created`;
    case "task_started": return "Task started";
    case "task_completed": return `Task completed: ${e.result_summary.slice(0, 100)}`;
    case "task_failed": return `Task failed: ${e.error}`;
    case "task_input_refreshed": return `Task input refreshed (${e.tool})`;
    case "tool_invoked": return `Tool invoked: ${e.tool}`;
    case "policy_denied": return `Policy denied [${e.rule}]: ${e.reason}`;
    case "policy_approval_requested": return `Approval requested [${e.rule}]: ${e.reason}`;
    case "policy_approval_granted": return "Approval granted";
    case "llm_requested": return `LLM request → ${e.provider}/${e.model}`;
    case "llm_responded": return `LLM ${e.latency_ms}ms (${e.prompt_tokens}+${e.completion_tokens} tok)`;
    case "llm_failed": return `LLM failed on ${e.provider}: ${e.error}`;
    case "skills_selected": return `Skills selected: ${e.skill_names.join(", ") || "none"}`;
    case "replan_requested": return `Replan requested (iter ${e.iteration})`;
    case "plan_revised": return `Plan revised — +${e.added_goals} goals (iter ${e.iteration})`;
    case "replan_cap_exceeded": return `Replan cap exceeded at iter ${e.iteration}`;
    case "mission_reflection_completed": return `Reflection completed (${e.skill_suggestion_count} skill suggestion${e.skill_suggestion_count === 1 ? "" : "s"})`;
    case "skill_proposal_written": return `Skill proposal written: ${e.skill_name}`;
    case "mission_cost_summary": return `Cost summary: ${e.llm_calls} calls, ${e.prompt_tokens}+${e.completion_tokens} tok, ${e.total_latency_ms}ms`;
    case "episodic_recall_surfaced": return `Episodic recall: ${e.prior_count} prior mission${e.prior_count === 1 ? "" : "s"} (kw: ${e.keywords.slice(0, 5).join(", ")})`;
    case "checkpoint_created": return `Checkpoint ${e.short_sha}: ${e.label}`;
    case "checkpoint_skipped": return `Checkpoint no-op (${e.tool}): ${e.reason}`;
    case "mcp_server_started": return `MCP ${e.name} started (${e.tools.length} tools)`;
    case "mcp_server_failed": return `MCP ${e.name} failed: ${e.error}`;
    case "mcp_tool_invoked": return `MCP ${e.server}/${e.tool} invoked`;
    case "skill_promoted": return `Skill promoted: ${e.name} v${e.version} (${e.sha.slice(0, 8)}, ${e.origin})`;
    case "skill_rolled_back": return `Skill rolled back: ${e.name} → ${e.to_sha.slice(0, 8)}${e.reason ? ` (${e.reason})` : ""}`;
    case "skill_retired": return `Skill retired: ${e.name} (${e.sha.slice(0, 8)}) — ${e.reason}`;
    case "skill_curation_suggested": return `Curator: ${e.kind} — ${e.name} (${e.evidence})`;
    case "skill_validation_passed": return `Skill validation passed: ${e.name}${e.soft_failures.length ? ` (warnings: ${e.soft_failures.join(", ")})` : ""}`;
    case "skill_validation_failed": return `Skill validation FAILED: ${e.name} — ${e.failed_checks.join(", ")}`;
    case "skill_auto_promoted": return `Skill AUTO-promoted: ${e.name} v${e.version} (${e.sha.slice(0, 8)})`;
    case "skill_auto_archived": return `Curator AUTO-archived: ${e.archived_name} (kept ${e.kept_name}, sim=${e.similarity.toFixed(3)}, rule=${e.rule})`;
    case "skill_merge_proposed": return `Curator merge proposal: ${e.merged_name} = ${e.source_a} + ${e.source_b} (sim=${e.body_similarity.toFixed(3)})`;
    case "mission_queued": return `Mission enqueued (queue_id=${e.queue_id})`;
    case "org_memory_recalled": return `Org memory recalled: ${e.block_preview.slice(0, 140)}${e.block_preview.length > 140 ? "…" : ""}`;
    case "org_memory_learned": return `Org memory learned (#${e.memory_id}): ${e.key}`;
    default: return JSON.stringify(e);
  }
}

function tone(e: ForgeEvent): string {
  const t = e.type;
  if (t.endsWith("_failed") || t === "policy_denied") return "text-forge-err";
  if (t.endsWith("_completed") || t === "policy_approval_granted") return "text-forge-success";
  if (t.startsWith("policy_")) return "text-forge-warn";
  if (t.startsWith("llm_")) return "text-forge-accent";
  if (t === "mission_cost_summary") return "text-forge-accent";
  if (t.startsWith("mcp_")) return "text-forge-muted";
  return "text-forge-fg";
}

const ALL_CATEGORIES: EventCategory[] = ["mission", "goal", "task", "llm", "plugin", "meta"];

function catColor(c: EventCategory): string {
  switch (c) {
    case "mission": return "#7c8cf8";
    case "goal":    return "#34d399";
    case "task":    return "#fbbf24";
    case "llm":     return "#a78bfa";
    case "plugin":  return "#38bdf8";
    default:        return "#8b93a7";
  }
}

export function EventTimeline() {
  const events = useEventsStore((s) => s.events);
  const goalToMission = useEventsStore((s) => s.goalToMission);
  const taskToGoal = useEventsStore((s) => s.taskToGoal);
  const selectedMissionId = useUiStore((s) => s.selectedMissionId);

  const [showAll, setShowAll] = useState(false);
  const [enabledCats, setEnabledCats] = useState<Set<EventCategory>>(
    () => new Set(ALL_CATEGORIES),
  );
  const [query, setQuery] = useState("");

  const missionFilter = showAll ? null : selectedMissionId;

  const filtered = useMemo(
    () =>
      filterEvents(events, {
        missionId: missionFilter,
        categories: enabledCats,
        query,
        goalToMission,
        taskToGoal,
      }),
    [events, missionFilter, enabledCats, query, goalToMission, taskToGoal],
  );

  const toggleCat = (c: EventCategory) => {
    setEnabledCats((prev) => {
      const next = new Set(prev);
      if (next.has(c)) next.delete(c);
      else next.add(c);
      return next;
    });
  };

  return (
    <Card className="flex flex-col overflow-hidden">
      <div className="px-3 py-2 border-b border-forge-border flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <div className="text-[11px] font-semibold uppercase tracking-wide text-forge-muted">
            Event stream
          </div>
          <div className="text-[10px] text-forge-faint tabular-nums">
            {filtered.length} / {events.length}
          </div>
        </div>
        <div className="flex items-center gap-2 flex-wrap">
          <label className="text-xs text-forge-muted flex items-center gap-1.5 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={showAll}
              onChange={(e) => setShowAll(e.target.checked)}
              className="accent-forge-accent"
            />
            Show all missions
          </label>
          {!showAll && (
            <span className="text-[10px] text-forge-faint font-mono">
              {selectedMissionId ? `${selectedMissionId.slice(0, 12)}…` : "no mission selected"}
            </span>
          )}
        </div>
        <div className="flex items-center gap-1 flex-wrap">
          {ALL_CATEGORIES.map((c) => {
            const on = enabledCats.has(c);
            return (
              <button
                key={c}
                onClick={() => toggleCat(c)}
                className={cn(
                  "text-[10px] uppercase tracking-wide px-2 py-0.5 rounded-full border transition-all flex items-center gap-1.5",
                  on
                    ? "border-forge-borderStrong bg-forge-panel2 text-forge-fg"
                    : "border-forge-border text-forge-faint hover:text-forge-muted",
                )}
                title={`Toggle ${c} events`}
              >
                <span
                  className="w-1.5 h-1.5 rounded-full transition-opacity"
                  style={{ background: catColor(c), opacity: on ? 1 : 0.3 }}
                />
                {c}
              </button>
            );
          })}
          <input
            type="text"
            placeholder="Filter…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="ml-auto text-xs bg-forge-bg/60 border border-forge-border rounded-md px-2 py-0.5 text-forge-fg w-28 focus:w-40 transition-all outline-none focus:border-forge-accent focus:ring-2 focus:ring-forge-accent/20 placeholder:text-forge-faint"
          />
        </div>
      </div>
      <ul className="overflow-y-auto max-h-full font-mono text-xs">
        {filtered.length === 0 && (
          <li className="px-3 py-3 text-forge-faint italic">
            {events.length === 0
              ? "Waiting for events…"
              : missionFilter
                ? "No events for this mission yet. Toggle 'Show all missions' to see everything."
                : "No events match the current filters."}
          </li>
        )}
        {filtered.map((env) => {
          const cat = eventCategory(env.event);
          return (
            <li
              key={env.seq}
              className="px-3 py-1.5 grid grid-cols-[46px_1fr] gap-2 items-baseline hover:bg-forge-panel2/40 transition-colors border-l-2 border-transparent hover:border-forge-border"
            >
              <span className="text-forge-faint tabular-nums text-[10px]">#{env.seq}</span>
              <span className={cn("truncate flex items-center gap-2", tone(env.event))} title={summarize(env.event)}>
                <span
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ background: catColor(cat) }}
                  title={cat}
                />
                <span className="truncate">{summarize(env.event)}</span>
              </span>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
