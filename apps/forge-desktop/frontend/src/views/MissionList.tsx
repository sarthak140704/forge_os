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
    <Card className="p-2.5 space-y-1.5 overflow-y-auto">
      <div className="flex items-center justify-between px-1.5 pt-1">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-forge-muted">Missions</div>
        {data && data.length > 0 && (
          <span className="text-[10px] text-forge-faint tabular-nums">{data.length}</span>
        )}
      </div>
      {isLoading && <div className="text-sm text-forge-muted px-1.5 py-1">Loading…</div>}
      {error && <div className="text-sm text-forge-err px-1.5 py-1">{String(error)}</div>}
      {data?.length === 0 && (
        <div className="text-sm text-forge-faint italic px-1.5 py-2">No missions yet.</div>
      )}
      <ul className="space-y-1">
        {data?.map((m: MissionSummary) => {
          const isActive = m.id === selectedId;
          const pct = m.goal_count > 0 ? Math.round((m.completed_goal_count / m.goal_count) * 100) : 0;
          return (
            <li key={m.id}>
              <button
                className={cn(
                  "group relative w-full text-left pl-3 pr-2.5 py-2 rounded-lg border transition-all",
                  isActive
                    ? "border-forge-accent/50 bg-forge-panel2 shadow-card"
                    : "border-transparent hover:border-forge-border hover:bg-forge-panel2/50"
                )}
                onClick={() => select(m.id)}
              >
                <span
                  className={cn(
                    "absolute left-0 top-1.5 bottom-1.5 w-0.5 rounded-full transition-all",
                    isActive ? "bg-accent-grad" : "bg-transparent group-hover:bg-forge-border"
                  )}
                />
                <div className="flex items-center justify-between gap-2">
                  <span className={cn("text-sm truncate", isActive ? "font-semibold text-forge-fg" : "font-medium")}>
                    {m.title}
                  </span>
                  <Badge tone={statusTone(m.status)} dot>{m.status}</Badge>
                </div>
                <div className="mt-1.5 flex items-center gap-2">
                  <div className="flex-1 h-1 rounded-full bg-forge-border/60 overflow-hidden">
                    <div
                      className={cn(
                        "h-full rounded-full transition-all",
                        m.status === "failed" || m.status === "cancelled" ? "bg-forge-err/70" : "bg-accent-grad"
                      )}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="text-[10px] text-forge-faint tabular-nums whitespace-nowrap">
                    {m.completed_goal_count}/{m.goal_count}
                  </span>
                </div>
              </button>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
