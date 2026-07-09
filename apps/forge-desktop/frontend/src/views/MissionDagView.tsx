import { useMemo, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Background,
  Controls,
  Handle,
  Position,
  ReactFlow,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import { getMission, cancelMission, extendMission } from "@/lib/ipc";
import { Badge, Button, Card } from "@/components/ui/primitives";
import { cn } from "@/lib/utils";
import type { Goal, GoalStatus, MissionDetail, MissionStatus } from "@/lib/events";

function statusColor(s: GoalStatus): string {
  switch (s) {
    case "completed": return "#34d399";
    case "running":   return "#7c8cf8";
    case "failed":    return "#f87171";
    case "skipped":   return "#8b93a7";
    case "ready":     return "#fbbf24";
    default:          return "#5b6273";
  }
}

type GoalNodeData = { title: string; status: GoalStatus };

function GoalNode({ data }: NodeProps) {
  const { title, status } = data as GoalNodeData;
  const color = statusColor(status);
  const running = status === "running";
  return (
    <div
      className="w-[200px] rounded-xl border bg-forge-panel/95 shadow-card px-3 py-2.5 transition-shadow"
      style={{ borderColor: running ? color : "#222634", boxShadow: running ? `0 0 0 1px ${color}55, 0 0 22px -8px ${color}` : undefined }}
    >
      <Handle type="target" position={Position.Top} />
      <div className="flex items-start gap-2">
        <span
          className={cn("mt-1 w-2 h-2 rounded-full shrink-0", running && "animate-pulse-dot")}
          style={{ background: color, boxShadow: `0 0 8px ${color}` }}
        />
        <div className="min-w-0">
          <div className="text-[12px] font-medium text-forge-fg leading-snug line-clamp-2">{title}</div>
          <div className="mt-0.5 text-[10px] uppercase tracking-wide" style={{ color }}>{status}</div>
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} />
    </div>
  );
}

const nodeTypes = { goal: GoalNode };

function missionTone(s: MissionStatus): "default" | "success" | "warn" | "err" | "info" {
  switch (s) {
    case "completed": return "success";
    case "failed":
    case "cancelled": return "err";
    case "running":
    case "planning": return "info";
    case "paused": return "warn";
    default: return "default";
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
    const y = (level.get(g.id) ?? 0) * 150;
    const col = columns.get(level.get(g.id) ?? 0) ?? 0;
    columns.set(level.get(g.id) ?? 0, col + 1);
    return {
      id: g.id,
      type: "goal",
      position: { x: col * 250, y },
      data: { title: g.title, status: g.status },
    };
  });
  const edges: Edge[] = goals.flatMap((g) =>
    g.depends_on.map((dep) => ({
      id: `${dep}->${g.id}`,
      source: dep,
      target: g.id,
      animated: g.status === "running",
      style: { stroke: "#3a4152" },
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
          <Badge tone={missionTone(status)} dot>{status}</Badge>
          <span className="text-forge-muted whitespace-nowrap tabular-nums">
            {data.goals.length} goals · {Object.values(data.tasks_by_goal).flat().length} tasks
          </span>
          {cancellable && (
            <Button
              size="sm"
              variant="danger"
              onClick={() => cancel.mutate()}
              disabled={cancel.isPending}
              title="Cancel this mission"
            >
              {cancel.isPending ? "Cancelling…" : "Cancel"}
            </Button>
          )}
          {extendable && (
            <Button
              size="sm"
              variant={showFollowUp ? "subtle" : "secondary"}
              onClick={() => setShowFollowUp((v) => !v)}
              title="Add a follow-up prompt to this mission"
            >
              {showFollowUp ? "Close" : "+ Follow-up"}
            </Button>
          )}
        </div>
      </div>
      {showFollowUp && extendable && (
        <div className="px-4 py-3 border-b border-forge-border bg-forge-panel2/60 flex flex-col gap-2">
          <textarea
            className="w-full min-h-[70px] bg-forge-bg border border-forge-border rounded-lg px-3 py-2 text-sm text-forge-fg placeholder:text-forge-faint focus:border-forge-accent focus:ring-2 focus:ring-forge-accent/25 outline-none resize-y transition"
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
            <Button
              size="sm"
              onClick={() => extend.mutate(followUp)}
              disabled={extend.isPending || !followUp.trim()}
            >
              {extend.isPending ? "Extending…" : "Extend mission"}
            </Button>
          </div>
        </div>
      )}
      <div className="flex-1 min-h-0 forge-flow">
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          fitView
          fitViewOptions={{ padding: 0.2 }}
          minZoom={0.2}
          maxZoom={2}
          proOptions={{ hideAttribution: true }}
        >
          <Background gap={22} size={1} color="#1a1e28" />
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
