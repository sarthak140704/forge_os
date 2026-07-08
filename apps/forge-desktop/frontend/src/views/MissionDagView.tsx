import { useMemo, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Background,
  Controls,
  ReactFlow,
  type Edge,
  type Node,
} from "@xyflow/react";
import { getMission, cancelMission, extendMission } from "@/lib/ipc";
import { Card } from "@/components/ui/primitives";
import type { Goal, GoalStatus, MissionDetail, MissionStatus } from "@/lib/events";

function statusColor(s: GoalStatus): string {
  switch (s) {
    case "completed": return "#34d399";
    case "running":   return "#7c8cf8";
    case "failed":    return "#f87171";
    case "skipped":   return "#8b93a7";
    case "ready":     return "#fbbf24";
    default:          return "#232732";
  }
}

function buildGraph(detail: MissionDetail): { nodes: Node[]; edges: Edge[] } {
  const goals: Goal[] = detail.goals;
  // Simple layered layout: level = longest-path-from-root; y = level*140, x = column*220.
  const idIndex = new Map(goals.map((g) => [g.id, g]));
  const level = new Map<string, number>();
  function lvl(id: string, seen = new Set<string>()): number {
    if (seen.has(id)) return 0;
    seen.add(id);
    if (level.has(id)) return level.get(id)!;
    const g = idIndex.get(id);
    if (!g || g.depends_on.length === 0) {
      level.set(id, 0);
      return 0;
    }
    const v = 1 + Math.max(...g.depends_on.map((d) => lvl(d, seen)));
    level.set(id, v);
    return v;
  }
  goals.forEach((g) => lvl(g.id));
  const columns = new Map<number, number>();
  const nodes: Node[] = goals.map((g) => {
    const y = (level.get(g.id) ?? 0) * 140;
    const col = columns.get(level.get(g.id) ?? 0) ?? 0;
    columns.set(level.get(g.id) ?? 0, col + 1);
    return {
      id: g.id,
      position: { x: col * 240, y },
      data: { label: `${g.title}\n(${g.status})` },
      style: {
        background: "#12151d",
        color: "#e5e7eb",
        border: `2px solid ${statusColor(g.status)}`,
        borderRadius: 8,
        padding: 8,
        fontSize: 12,
        whiteSpace: "pre-line",
        width: 200,
      },
    };
  });
  const edges: Edge[] = goals.flatMap((g) =>
    g.depends_on.map((dep) => ({
      id: `${dep}->${g.id}`,
      source: dep,
      target: g.id,
      animated: g.status === "running",
      style: { stroke: "#8b93a7" },
    }))
  );
  return { nodes, edges };
}

export function MissionDagView({ missionId }: { missionId: string }) {
  const qc = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["mission", missionId],
    queryFn: () => getMission(missionId),
    refetchInterval: 2_000,
  });

  const cancel = useMutation({
    mutationFn: () => cancelMission(missionId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mission", missionId] }),
  });
  const extend = useMutation({
    mutationFn: (prompt: string) => extendMission(missionId, prompt),
    onSuccess: () => {
      setFollowUp("");
      setShowFollowUp(false);
      qc.invalidateQueries({ queryKey: ["mission", missionId] });
      qc.invalidateQueries({ queryKey: ["missions"] });
    },
  });

  const [followUp, setFollowUp] = useState("");
  const [showFollowUp, setShowFollowUp] = useState(false);

  const { nodes, edges } = useMemo(() => {
    if (!data) return { nodes: [], edges: [] };
    return buildGraph(data);
  }, [data]);

  if (isLoading) return <div className="text-sm text-forge-muted p-3">Loading DAG…</div>;
  if (error)     return <div className="text-sm text-forge-err p-3">{String(error)}</div>;
  if (!data)     return null;

  const status: MissionStatus = data.mission.status;
  const cancellable = status === "running" || status === "planning" || status === "ready" || status === "paused";
  const extendable  = status === "completed" || status === "failed" || status === "cancelled";

  return (
    <Card className="flex-1 flex flex-col overflow-hidden">
      <div className="px-4 py-2 border-b border-forge-border flex items-center justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-semibold truncate">{data.mission.title}</div>
          <div className="text-xs text-forge-muted truncate">{data.mission.description}</div>
        </div>
        <div className="flex items-center gap-2 text-xs">
          <span className="text-forge-muted whitespace-nowrap">
            {data.goals.length} goals · {Object.values(data.tasks_by_goal).flat().length} tasks
          </span>
          {cancellable && (
            <button
              onClick={() => cancel.mutate()}
              disabled={cancel.isPending}
              className="px-2 py-1 rounded border border-forge-err text-forge-err hover:bg-forge-err hover:text-forge-bg transition disabled:opacity-50"
              title="Cancel this mission"
            >
              {cancel.isPending ? "Cancelling…" : "Cancel"}
            </button>
          )}
          {extendable && (
            <button
              onClick={() => setShowFollowUp((v) => !v)}
              className="px-2 py-1 rounded border border-forge-accent text-forge-accent hover:bg-forge-accent hover:text-forge-bg transition"
              title="Add a follow-up prompt to this mission"
            >
              {showFollowUp ? "Cancel" : "+ Follow-up"}
            </button>
          )}
        </div>
      </div>
      {showFollowUp && extendable && (
        <div className="px-4 py-3 border-b border-forge-border bg-forge-card/50 flex flex-col gap-2">
          <textarea
            className="w-full min-h-[70px] bg-forge-bg border border-forge-border rounded px-2 py-1 text-sm text-forge-fg placeholder:text-forge-muted focus:border-forge-accent outline-none resize-y"
            placeholder="What should this mission also do? e.g. 'now also write a README'"
            value={followUp}
            onChange={(e) => setFollowUp(e.target.value)}
            disabled={extend.isPending}
            autoFocus
          />
          <div className="flex items-center gap-2 justify-end">
            {extend.isError && (
              <span className="text-xs text-forge-err mr-auto truncate">
                {String((extend.error as Error).message ?? extend.error)}
              </span>
            )}
            <button
              onClick={() => extend.mutate(followUp)}
              disabled={extend.isPending || !followUp.trim()}
              className="px-3 py-1 rounded bg-forge-accent text-forge-bg text-xs font-medium hover:opacity-90 transition disabled:opacity-50"
            >
              {extend.isPending ? "Extending…" : "Extend mission"}
            </button>
          </div>
        </div>
      )}
      <div className="flex-1 min-h-0 forge-flow">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          fitView
          fitViewOptions={{ padding: 0.2 }}
          minZoom={0.2}
          maxZoom={2}
          proOptions={{ hideAttribution: true }}
        >
          <Background gap={20} color="#1a1e28" />
          <Controls
            position="bottom-left"
            showInteractive={false}
            className="!bg-forge-panel !border !border-forge-border !rounded-md !shadow-lg"
          />
        </ReactFlow>
      </div>
    </Card>
  );
}
