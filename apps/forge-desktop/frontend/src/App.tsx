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
import { Button, Card } from "@/components/ui/primitives";
import { cn } from "@/lib/utils";

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
    <div className="h-full grid grid-rows-[auto_1fr] bg-forge-radial text-forge-fg">
      <header className="px-5 py-3 border-b border-forge-border/80 flex items-center justify-between backdrop-blur-sm">
        <div className="flex items-center gap-3">
          <div className="relative w-8 h-8 rounded-lg bg-accent-grad grid place-items-center shadow-glow">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" className="text-white">
              <path d="M4 14l7-9v6h5l-7 9v-6H4z" fill="currentColor" />
            </svg>
          </div>
          <div className="leading-tight">
            <div className="text-[15px] font-semibold tracking-tighter2">Forge OS</div>
            <div className="text-[11px] text-forge-muted">Autonomous SWE runtime</div>
          </div>
        </div>
        <div className="flex items-center gap-2.5">
          <StatusPill bootError={bootError} ready={ready} />
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setShowSettings(true)}
            title="Settings"
            aria-label="Settings"
            className="w-8 h-8 px-0 text-base"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
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
            : <EmptyDag />}
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

function StatusPill({ bootError, ready }: { bootError: string | null; ready: boolean }) {
  const { label, dot, text } = bootError
    ? { label: "runtime error", dot: "bg-forge-err", text: "text-forge-err" }
    : ready
      ? { label: "runtime ready", dot: "bg-forge-success", text: "text-forge-success" }
      : { label: "reconnecting", dot: "bg-forge-warn", text: "text-forge-warn" };
  return (
    <span className="inline-flex items-center gap-2 pl-2 pr-2.5 py-1 rounded-full bg-forge-panel2/70 border border-forge-border text-[11px] font-medium">
      <span className="relative flex w-2 h-2">
        {!bootError && ready && (
          <span className={cn("absolute inline-flex w-full h-full rounded-full opacity-60 animate-ping", dot)} />
        )}
        <span className={cn("relative inline-flex w-2 h-2 rounded-full", dot, !ready && !bootError && "animate-pulse-dot")} />
      </span>
      <span className={text}>{label}</span>
    </span>
  );
}

function EmptyDag() {
  return (
    <Card className="flex-1 flex flex-col items-center justify-center text-center gap-3 text-forge-muted">
      <div className="w-12 h-12 rounded-xl bg-forge-panel2 border border-forge-border grid place-items-center">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" className="text-forge-faint">
          <circle cx="6" cy="6" r="2.5" /><circle cx="18" cy="6" r="2.5" /><circle cx="12" cy="18" r="2.5" />
          <path d="M7.7 7.7 10.5 16M16.3 7.7 13.5 16" />
        </svg>
      </div>
      <div className="text-sm text-forge-fg font-medium">No mission selected</div>
      <div className="text-xs max-w-[260px]">Create a mission or pick one from the list to watch its goal DAG plan and execute in real time.</div>
    </Card>
  );
}
