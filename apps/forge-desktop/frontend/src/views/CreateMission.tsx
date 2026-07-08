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
      <div className="text-sm font-semibold text-forge-fg">New mission</div>
      <input
        className="w-full bg-forge-bg border border-forge-border rounded-md px-3 py-2 text-sm outline-none focus:border-forge-accent transition"
        placeholder="Title — short imperative sentence"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        className="w-full bg-forge-bg border border-forge-border rounded-md px-3 py-2 text-sm font-mono outline-none focus:border-forge-accent transition resize-y"
        rows={4}
        placeholder="Describe the desired outcome + any constraints. The planner will decompose this into a goal DAG."
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      {error && <div className="text-xs text-forge-err whitespace-pre-wrap">{error}</div>}
      <div className="flex justify-end">
        <Button
          disabled={submit.isPending || title.trim() === ""}
          onClick={() => submit.mutate()}
        >
          {submit.isPending ? "Planning…" : "Plan & run"}
        </Button>
      </div>
    </Card>
  );
}
