import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { CreateMission } from "@/views/CreateMission";
import { MissionList } from "@/views/MissionList";
import { MissionDagView } from "@/views/MissionDagView";
import { EventTimeline } from "@/views/EventTimeline";
import { Settings } from "@/views/Settings";
import {
  replayEvents,
  runtimeStatus,
  subscribeEvents,
  subscribeRuntimeError,
  subscribeRuntimeReady,
} from "@/lib/ipc";
import { useEventsStore } from "@/stores/events";
import { useUiStore } from "@/stores/ui";
import { Badge, Button, Card } from "@/components/ui/primitives";

export default function App() {
  const selectedId = useUiStore((s) => s.selectedMissionId);
  const push = useEventsStore((s) => s.push);
  const hydrate = useEventsStore((s) => s.hydrate);
  const qc = useQueryClient();
  const [ready, setReady] = useState(false);
  const [bootError, setBootError] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let cancelled = false;
    let readyRef = false; // avoid stale-closure comparison in heartbeat

    const markReady = async () => {
      if (cancelled) return;
      const wasReady = readyRef;
      readyRef = true;
      setReady(true);
      setBootError(null);
      try {
        const backlog = await replayEvents();
        hydrate(backlog);
      } catch { /* backlog fetch is best-effort */ }
      qc.invalidateQueries();
      // On reconnect (backend restarted): loudly refresh so any mid-flight
      // events dropped during the downtime are re-hydrated from the store.
      if (wasReady === false) { /* first ready — nothing extra */ }
    };

    const markDown = () => {
      if (cancelled) return;
      readyRef = false;
      setReady(false);
    };

    (async () => {
      // Wire up subscriptions first so no live events are dropped.
      unlisteners.push(await subscribeRuntimeReady(markReady));
      unlisteners.push(await subscribeRuntimeError((err) => setBootError(err)));
      unlisteners.push(await subscribeEvents((env) => {
        push(env);
        qc.invalidateQueries({ queryKey: ["missions"] });
        if (selectedId) qc.invalidateQueries({ queryKey: ["mission", selectedId] });
      }));

      // Poll-based fallback: the ready broadcast fires ~50ms after Rust boots
      // and may land before the listener above is attached. Retry the IPC
      // status probe until it succeeds (state managed) or we give up.
      for (let attempt = 0; attempt < 60 && !cancelled; attempt++) {
        try {
          await runtimeStatus();
          await markReady();
          break;
        } catch {
          await new Promise((r) => setTimeout(r, 250));
        }
      }
    })();

    // Heartbeat: detects backend hot-reload/crash. Tauri dev restarts the
    // Rust process on every crate edit; without this the badge would lie.
    const heartbeat = window.setInterval(async () => {
      if (cancelled) return;
      try {
        await runtimeStatus();
        if (!readyRef) await markReady();
      } catch {
        if (readyRef) markDown();
      }
    }, 2000);

    return () => {
      cancelled = true;
      window.clearInterval(heartbeat);
      unlisteners.forEach((u) => { try { u(); } catch { /* ignore */ } });
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="h-full grid grid-rows-[auto_1fr] bg-forge-bg text-forge-fg">
      <header className="px-6 py-3 border-b border-forge-border flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="w-6 h-6 rounded bg-forge-accent" />
          <div>
            <div className="text-base font-semibold">Forge OS</div>
            <div className="text-xs text-forge-muted">Autonomous SWE runtime · Phase 1 vertical slice</div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {bootError
            ? <Badge tone="err">runtime error</Badge>
            : ready
              ? <Badge tone="success">runtime ready</Badge>
              : <Badge tone="warn">reconnecting…</Badge>}
          <Button variant="ghost" onClick={() => setShowSettings(true)} title="Settings">
            ⚙
          </Button>
        </div>
      </header>

      {showSettings && <Settings onClose={() => setShowSettings(false)} />}

      <main className="grid grid-cols-[300px_minmax(0,1fr)_400px] gap-3 p-3 min-h-0 overflow-hidden">
        <aside className="grid grid-rows-[auto_1fr] gap-3 min-h-0 min-w-0">
          <CreateMission />
          <MissionList />
        </aside>

        <section className="min-h-0 min-w-0 flex flex-col">
          {selectedId
            ? <MissionDagView missionId={selectedId} />
            : <Card className="flex-1 flex items-center justify-center text-sm text-forge-muted">
                Select a mission (or create one) to see its goal DAG.
              </Card>}
          {bootError && (
            <Card className="mt-3 p-3 text-xs text-forge-err whitespace-pre-wrap">
              {bootError}
            </Card>
          )}
        </section>

        <aside className="min-h-0 min-w-0 flex">
          <EventTimeline />
        </aside>
      </main>
    </div>
  );
}
