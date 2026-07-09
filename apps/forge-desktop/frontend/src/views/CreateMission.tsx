import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Card } from "@/components/ui/primitives";
import { createMission, planAndRun } from "@/lib/ipc";
import { useUiStore } from "@/stores/ui";

export function CreateMission() {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [error, setError] = useState<string | null>(null);
  const qc = useQueryClient();
  const select = useUiStore((s) => s.select);

  const submit = useMutation({
    mutationFn: async () => {
      const id = await createMission(title, description);
      // Select the new mission immediately so the DAG view opens
      // without a manual click. plan_and_run runs in background.
      select(id);
      await planAndRun(id);
      return id;
    },
    onSuccess: () => {
      setTitle("");
      setDescription("");
      setError(null);
      qc.invalidateQueries({ queryKey: ["missions"] });
    },
    onError: (e) => setError(String(e)),
  });

  return (
    <Card className="p-4 space-y-3">
      <div className="flex items-center gap-2">
        <div className="w-6 h-6 rounded-md bg-forge-panel2 border border-forge-border grid place-items-center text-forge-accent">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M12 5v14M5 12h14" />
          </svg>
        </div>
        <div className="text-sm font-semibold text-forge-fg">New mission</div>
      </div>
      <input
        className="w-full bg-forge-bg/80 border border-forge-border rounded-lg px-3 py-2 text-sm outline-none focus:border-forge-accent focus:ring-2 focus:ring-forge-accent/25 transition placeholder:text-forge-faint"
        placeholder="Title — short imperative sentence"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        className="w-full bg-forge-bg/80 border border-forge-border rounded-lg px-3 py-2 text-sm font-mono outline-none focus:border-forge-accent focus:ring-2 focus:ring-forge-accent/25 transition resize-y placeholder:text-forge-faint leading-relaxed"
        rows={4}
        placeholder="Describe the desired outcome + any constraints. The planner will decompose this into a goal DAG."
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && title.trim() && !submit.isPending) {
            submit.mutate();
          }
        }}
      />
      {error && <div className="text-xs text-forge-err whitespace-pre-wrap">{error}</div>}
      <div className="flex items-center justify-between">
        <span className="text-[10px] text-forge-faint select-none">⌘/Ctrl + Enter</span>
        <Button
          disabled={submit.isPending || title.trim() === ""}
          onClick={() => submit.mutate()}
        >
          {submit.isPending
            ? <><Spinner /> Planning…</>
            : <>Plan &amp; run</>}
        </Button>
      </div>
    </Card>
  );
}

function Spinner() {
  return (
    <svg className="animate-spin" width="13" height="13" viewBox="0 0 24 24" fill="none">
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="3" opacity="0.25" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}
