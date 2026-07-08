import { useQuery } from "@tanstack/react-query";
import { Badge, Card } from "@/components/ui/primitives";
import { listMissions } from "@/lib/ipc";
import { useUiStore } from "@/stores/ui";
import type { MissionStatus, MissionSummary } from "@/lib/events";
import { cn } from "@/lib/utils";

function statusTone(s: MissionStatus): "default" | "success" | "warn" | "err" | "info" {
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

export function MissionList() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["missions"],
    queryFn: listMissions,
    refetchInterval: 3_000,
  });
  const selectedId = useUiStore((s) => s.selectedMissionId);
  const select = useUiStore((s) => s.select);

  return (
    <Card className="p-3 space-y-2 overflow-y-auto">
      <div className="text-xs uppercase tracking-wide text-forge-muted px-1">Missions</div>
      {isLoading && <div className="text-sm text-forge-muted">Loading…</div>}
      {error && <div className="text-sm text-forge-err">{String(error)}</div>}
      {data?.length === 0 && (
        <div className="text-sm text-forge-muted italic px-1">No missions yet.</div>
      )}
      <ul className="space-y-1">
        {data?.map((m: MissionSummary) => {
          const isActive = m.id === selectedId;
          return (
            <li key={m.id}>
              <button
                className={cn(
                  "w-full text-left px-2 py-2 rounded-md hover:bg-forge-bg transition-colors border",
                  isActive
                    ? "border-forge-accent bg-forge-bg"
                    : "border-transparent"
                )}
                onClick={() => select(m.id)}
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="font-medium truncate">{m.title}</span>
                  <Badge tone={statusTone(m.status)}>{m.status}</Badge>
                </div>
                <div className="text-xs text-forge-muted mt-1">
                  {m.completed_goal_count}/{m.goal_count} goals · {new Date(m.created_at).toLocaleTimeString()}
                </div>
              </button>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
