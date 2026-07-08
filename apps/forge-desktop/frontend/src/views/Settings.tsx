import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import { Badge, Button, Card } from "@/components/ui/primitives";
import {
  deleteSecret,
  exportAudit,
  listCheckpoints,
  listSecretStatus,
  revertCheckpoint,
  setSecret,
  type Checkpoint,
  type SecretStatus,
} from "@/lib/ipc";
import { useUiStore } from "@/stores/ui";

/**
 * Settings panel — Phase-3 governance UI.
 *   • Secrets manager (OS keyring; env vars still take precedence)
 *   • Shadow-git checkpoints (list + revert, scoped to current mission)
 *   • Audit-bundle export (JSON dump of every table)
 *
 * Rendered as a modal from the header cog. All destructive actions
 * require an explicit confirm.
 */
export function Settings({ onClose }: { onClose: () => void }) {
  return (
    <div
      className="fixed inset-0 z-40 bg-black/60 flex items-center justify-center p-6"
      onClick={onClose}
    >
      <div
        className="bg-forge-bg border border-forge-border rounded-lg w-full max-w-3xl max-h-[90vh] overflow-hidden flex flex-col"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="px-5 py-3 border-b border-forge-border flex items-center justify-between">
          <div>
            <div className="text-base font-semibold">Settings</div>
            <div className="text-xs text-forge-muted">
              Governance · secrets · checkpoints · audit
            </div>
          </div>
          <Button variant="ghost" onClick={onClose}>Close</Button>
        </header>

        <div className="flex-1 overflow-y-auto p-5 space-y-6">
          <SecretsSection />
          <CheckpointsSection />
          <AuditSection />
        </div>
      </div>
    </div>
  );
}

// ---------- Secrets ----------

function SecretsSection() {
  const qc = useQueryClient();
  const secrets = useQuery({
    queryKey: ["secrets"],
    queryFn: listSecretStatus,
    refetchOnWindowFocus: false,
  });

  const invalidate = () => qc.invalidateQueries({ queryKey: ["secrets"] });

  return (
    <Section
      title="API keys (OS keyring)"
      hint="Env vars always win. Keyring-stored keys persist across restarts."
    >
      {secrets.isLoading && <Muted>Loading…</Muted>}
      {secrets.data && (
        <div className="space-y-2">
          {secrets.data.map((s) => (
            <SecretRow key={s.name} secret={s} onChange={invalidate} />
          ))}
        </div>
      )}
    </Section>
  );
}

function SecretRow({
  secret,
  onChange,
}: {
  secret: SecretStatus;
  onChange: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);

  const tone =
    secret.source === "env" ? "info" :
    secret.source === "keyring" ? "success" : "default";

  const save = async () => {
    if (!value.trim()) return;
    setBusy(true);
    try {
      await setSecret(secret.name, value.trim());
      setValue("");
      setEditing(false);
      onChange();
    } catch (e) {
      alert(`Failed to save: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const del = async () => {
    if (!confirm(`Delete ${secret.name} from keyring? (env vars are unaffected.)`)) return;
    setBusy(true);
    try {
      await deleteSecret(secret.name);
      onChange();
    } catch (e) {
      alert(`Failed to delete: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-3 p-2 rounded border border-forge-border bg-forge-panel/40">
      <div className="flex-1 min-w-0">
        <div className="text-sm font-mono truncate">{secret.name}</div>
        <div className="text-xs text-forge-muted">
          <Badge tone={tone}>{secret.source}</Badge>
        </div>
      </div>
      {editing ? (
        <>
          <input
            type="password"
            placeholder="paste value…"
            value={value}
            onChange={(e) => setValue(e.target.value)}
            className="w-56 px-2 py-1 text-sm bg-forge-bg border border-forge-border rounded font-mono"
            autoFocus
          />
          <Button onClick={save} disabled={busy || !value.trim()}>Save</Button>
          <Button variant="ghost" onClick={() => { setEditing(false); setValue(""); }}>Cancel</Button>
        </>
      ) : (
        <>
          <Button variant="ghost" onClick={() => setEditing(true)} disabled={busy}>
            {secret.source === "keyring" ? "Replace" : "Set"}
          </Button>
          {secret.source === "keyring" && (
            <Button variant="danger" onClick={del} disabled={busy}>Delete</Button>
          )}
        </>
      )}
    </div>
  );
}

// ---------- Checkpoints ----------

function CheckpointsSection() {
  const missionId = useUiStore((s) => s.selectedMissionId) ?? undefined;
  const qc = useQueryClient();
  const cps = useQuery({
    queryKey: ["checkpoints", missionId ?? "all"],
    queryFn: () => listCheckpoints(missionId, 50),
    refetchOnWindowFocus: false,
  });

  const invalidate = () => qc.invalidateQueries({ queryKey: ["checkpoints"] });

  return (
    <Section
      title={missionId ? `Checkpoints (current mission)` : "Checkpoints (all)"}
      hint="Auto-snapshotted after every mutating tool. Revert is destructive."
    >
      {cps.isLoading && <Muted>Loading…</Muted>}
      {cps.data?.length === 0 && (
        <Muted>
          No checkpoints yet. Run a mission that writes files.
        </Muted>
      )}
      {cps.data && cps.data.length > 0 && (
        <div className="space-y-2 max-h-72 overflow-y-auto">
          {cps.data.map((cp) => (
            <CheckpointRow key={cp.sha} cp={cp} onReverted={invalidate} />
          ))}
        </div>
      )}
    </Section>
  );
}

function CheckpointRow({ cp, onReverted }: { cp: Checkpoint; onReverted: () => void }) {
  const [busy, setBusy] = useState(false);
  const revert = async () => {
    if (!confirm(
      `Revert workspace to ${cp.short_sha}?\n\n"${cp.subject}"\n\nThis discards ALL changes made after this checkpoint.`
    )) return;
    setBusy(true);
    try {
      await revertCheckpoint(cp.sha);
      onReverted();
    } catch (e) {
      alert(`Revert failed: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="p-2 rounded border border-forge-border bg-forge-panel/40 space-y-1">
      <div className="flex items-center gap-2">
        <span className="text-xs font-mono text-forge-accent">{cp.short_sha}</span>
        <span className="text-xs text-forge-muted">
          {new Date(cp.timestamp).toLocaleString()}
        </span>
        {cp.tool && <Badge tone="info">{cp.tool}</Badge>}
        <div className="flex-1" />
        <Button variant="danger" onClick={revert} disabled={busy}>Revert</Button>
      </div>
      <div className="text-sm">{cp.subject}</div>
      <div className="text-xs text-forge-muted font-mono">
        +{cp.insertions} −{cp.deletions} · {cp.files_changed} file{cp.files_changed === 1 ? "" : "s"}
      </div>
    </div>
  );
}

// ---------- Audit ----------

function AuditSection() {
  const [result, setResult] = useState<null | { path: string; total: number }>(null);
  const exportMut = useMutation({
    mutationFn: async () => {
      const dest = await save({
        title: "Export audit bundle",
        defaultPath: `forge-audit-${new Date().toISOString().replace(/[:.]/g, "-")}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!dest) return null;
      const res = await exportAudit(dest);
      const total = res.missions + res.goals + res.tasks + res.events + res.reflections;
      return { path: res.path, total };
    },
    onSuccess: (r) => { if (r) setResult(r); },
    onError: (e) => alert(`Export failed: ${String(e)}`),
  });

  return (
    <Section
      title="Audit bundle"
      hint="Dumps every mission, goal, task, event, and reflection to JSON."
    >
      <div className="flex items-center gap-3">
        <Button onClick={() => exportMut.mutate()} disabled={exportMut.isPending}>
          {exportMut.isPending ? "Exporting…" : "Export JSON…"}
        </Button>
        {result && (
          <div className="text-xs text-forge-muted truncate">
            Wrote {result.total.toLocaleString()} rows → <span className="font-mono">{result.path}</span>
          </div>
        )}
      </div>
    </Section>
  );
}

// ---------- Utils ----------

function Section({ title, hint, children }: { title: string; hint?: string; children: ReactNode }) {
  return (
    <Card className="p-4 space-y-3">
      <div>
        <div className="text-sm font-semibold">{title}</div>
        {hint && <div className="text-xs text-forge-muted mt-0.5">{hint}</div>}
      </div>
      {children}
    </Card>
  );
}

function Muted({ children }: { children: ReactNode }) {
  return <div className="text-xs text-forge-muted italic">{children}</div>;
}
