import { useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { save } from "@tauri-apps/plugin-dialog";
import { Badge, Button, Card } from "@/components/ui/primitives";
import {
  deleteSecret,
  exportAudit,
  listActiveSkills,
  listCheckpoints,
  listSecretStatus,
  listSkillProposals,
  listSkillVersions,
  rejectSkillProposal,
  retireSkill,
  revertCheckpoint,
  rollbackSkill,
  runCurator,
  curatorScan,
  setSecret,
  validateSkillProposal,
  queueStatus,
  listOrgMemory,
  deleteOrgMemory,
  type Checkpoint,
  type CuratorSuggestion,
  type CuratorReport,
  type SecretStatus,
  type SkillProposalSummary,
  type SkillVersion,
  type ValidationReport,
  type OrgMemoryRow,
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
          <QueueSection />
          <MemorySection />
          <CheckpointsSection />
          <SkillsSection />
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

// ---------- Queue (Phase 4d) ----------

function QueueSection() {
  const qc = useQueryClient();
  const status = useQuery({
    queryKey: ["queue-status"],
    queryFn: queueStatus,
    refetchInterval: 4000,
    refetchOnWindowFocus: false,
  });

  const invalidate = () => qc.invalidateQueries({ queryKey: ["queue-status"] });

  const workers = status.data?.workers ?? 0;
  const workersOn = workers > 0;

  return (
    <Section
      title="Mission Queue"
      hint={workersOn
        ? `Worker pool active (${workers} workers). Missions can be enqueued for background execution and survive restarts.`
        : "Worker pool disabled (workers=0 in RuntimeConfig). Missions still run inline via plan_and_run."}
    >
      {status.isLoading && <Muted>Loading…</Muted>}
      {status.data && (
        <>
          <div className="flex items-center gap-3 mb-3">
            <Badge tone={workersOn ? "success" : "info"}>
              {workersOn ? `${workers} workers` : "inline mode"}
            </Badge>
            <span className="text-sm">
              <span className="text-forge-muted">queued</span>{" "}
              <span className="font-mono">{status.data.queued}</span>
            </span>
            <span className="text-sm">
              <span className="text-forge-muted">claimed</span>{" "}
              <span className="font-mono">{status.data.claimed}</span>
            </span>
            <div className="flex-1" />
            <Button variant="ghost" onClick={invalidate}>Refresh</Button>
          </div>
          {status.data.recent.length === 0 ? (
            <Muted>No recent queue entries.</Muted>
          ) : (
            <div className="space-y-1 max-h-64 overflow-y-auto">
              {status.data.recent.map((row) => (
                <div
                  key={row.id}
                  className="p-2 rounded border border-forge-border bg-forge-panel/40 text-xs font-mono flex items-center gap-2"
                >
                  <span className="text-forge-muted">#{row.id}</span>
                  <Badge tone={queueTone(row.status)}>{row.status}</Badge>
                  <span className="truncate flex-1" title={row.mission_id}>
                    {row.mission_id.slice(0, 8)}…
                  </span>
                  <span className="text-forge-muted">
                    {new Date(row.enqueued_at).toLocaleTimeString()}
                  </span>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </Section>
  );
}

function queueTone(s: string): "info" | "success" | "warn" | "err" {
  if (s === "done") return "success";
  if (s === "failed") return "err";
  if (s === "claimed") return "warn";
  return "info";
}

// ---------- Organizational Memory (Phase 4f) ----------

function MemorySection() {
  const qc = useQueryClient();
  const rows = useQuery({
    queryKey: ["org-memory"],
    queryFn: () => listOrgMemory(200),
    refetchOnWindowFocus: false,
  });

  const del = useMutation({
    mutationFn: (id: number) => deleteOrgMemory(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["org-memory"] }),
  });

  return (
    <Section
      title="Organizational Memory"
      hint="Insights promoted from mission reflections. Injected into every planner prompt as durable cross-mission context."
    >
      {rows.isLoading && <Muted>Loading…</Muted>}
      {rows.data?.length === 0 && (
        <Muted>
          No memory yet. Complete a mission — its reflection insights are promoted here automatically.
        </Muted>
      )}
      {rows.data && rows.data.length > 0 && (
        <div className="space-y-2 max-h-80 overflow-y-auto">
          {rows.data.map((row) => (
            <MemoryRow
              key={row.id}
              row={row}
              onDelete={() => {
                if (confirm(`Retire memory #${row.id}?\n\n"${row.key}"`)) {
                  del.mutate(row.id);
                }
              }}
            />
          ))}
        </div>
      )}
    </Section>
  );
}

function MemoryRow({ row, onDelete }: { row: OrgMemoryRow; onDelete: () => void }) {
  return (
    <div className="p-2 rounded border border-forge-border bg-forge-panel/40 space-y-1">
      <div className="flex items-center gap-2">
        <span className="text-xs font-mono text-forge-accent">#{row.id}</span>
        <span className="text-xs text-forge-muted">
          {new Date(row.created_at).toLocaleString()}
        </span>
        {row.tags.slice(0, 4).map((t) => (
          <Badge key={t} tone="info">{t}</Badge>
        ))}
        <div className="flex-1" />
        <Button variant="danger" onClick={onDelete}>Retire</Button>
      </div>
      <div className="text-sm font-medium">{row.key}</div>
      <div className="text-xs text-forge-muted whitespace-pre-wrap">{row.value}</div>
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

// ---------- Skills (Phase 4a/4b) ----------

function SkillsSection() {
  const qc = useQueryClient();
  const active     = useQuery({ queryKey: ["skills-active"], queryFn: listActiveSkills });
  const proposals  = useQuery({ queryKey: ["skills-proposals"], queryFn: listSkillProposals });
  const [suggestions, setSuggestions] = useState<CuratorSuggestion[] | null>(null);
  const [report, setReport] = useState<CuratorReport | null>(null);
  const [busy, setBusy] = useState(false);

  const invalidate = () => {
    qc.invalidateQueries({ queryKey: ["skills-active"] });
    qc.invalidateQueries({ queryKey: ["skills-proposals"] });
  };

  const doRunCurator = async () => {
    setBusy(true);
    try { setSuggestions(await runCurator()); setReport(null); }
    catch (e) { alert(`Curator failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  const doScan = async (apply: boolean) => {
    setBusy(true);
    try {
      const r = await curatorScan(apply);
      setReport(r);
      setSuggestions(r.suggestions);
      if (apply) invalidate();
    } catch (e) { alert(`Scan failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  return (
    <Section
      title="Skills"
      hint="Version-controlled procedural knowledge. Every promotion is validated; every rollback is bit-exact."
    >
      <div className="flex items-center gap-2 flex-wrap">
        <Button variant="ghost" onClick={invalidate}>Refresh</Button>
        <Button variant="ghost" onClick={doRunCurator} disabled={busy}>Advisory scan</Button>
        <Button variant="ghost" onClick={() => doScan(false)} disabled={busy}>Scan (dry-run)</Button>
        <Button
          onClick={() => {
            if (confirm("Apply curator actions? This will archive duplicate skills and drop merge proposals into `proposed/`. Both are reversible via rollback / rejecting the proposal.")) {
              doScan(true);
            }
          }}
          disabled={busy}
        >
          Scan & apply
        </Button>
      </div>

      {/* Proposals */}
      <div>
        <div className="text-xs font-semibold text-forge-muted mb-2">
          Pending proposals ({proposals.data?.length ?? 0})
        </div>
        {proposals.isLoading && <Muted>Loading…</Muted>}
        {proposals.data?.length === 0 && <Muted>No proposals awaiting review.</Muted>}
        <div className="space-y-2">
          {proposals.data?.map((p) => (
            <ProposalRow key={p.filename} proposal={p} onChanged={invalidate} />
          ))}
        </div>
      </div>

      {/* Active */}
      <div>
        <div className="text-xs font-semibold text-forge-muted mb-2">
          Active skills ({active.data?.length ?? 0})
        </div>
        {active.isLoading && <Muted>Loading…</Muted>}
        {active.data?.length === 0 && <Muted>No active skills yet.</Muted>}
        <div className="space-y-2">
          {active.data?.map((s) => (
            <ActiveSkillRow key={s.name} skill={s} onChanged={invalidate} />
          ))}
        </div>
      </div>

      {/* Curator report — actions taken */}
      {report && (report.auto_archived.length > 0 || report.merge_proposals.length > 0) && (
        <div>
          <div className="text-xs font-semibold text-forge-muted mb-2">
            Curator actions ({report.auto_archived.length + report.merge_proposals.length})
          </div>
          <div className="space-y-1">
            {report.auto_archived.map(([lost, kept], i) => (
              <div key={`arch-${i}`} className="text-xs">
                <Badge tone="warn">archived</Badge>
                <span className="ml-2 font-mono">{lost}</span>
                <span className="ml-2 text-forge-muted">— kept `{kept}`</span>
              </div>
            ))}
            {report.merge_proposals.map((f, i) => (
              <div key={`merge-${i}`} className="text-xs">
                <Badge tone="info">merge proposal</Badge>
                <span className="ml-2 font-mono">{f}</span>
                <span className="ml-2 text-forge-muted">— review under Pending proposals above</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Curator suggestions */}
      {suggestions && (
        <div>
          <div className="text-xs font-semibold text-forge-muted mb-2">
            Curator suggestions ({suggestions.length})
          </div>
          {suggestions.length === 0 && <Muted>No suggestions — the catalog looks clean.</Muted>}
          <div className="space-y-2">
            {suggestions.map((s, i) => (
              <div key={i} className="p-2 rounded border border-forge-border bg-forge-panel/40 text-xs">
                <Badge tone={s.kind === "duplicate" ? "warn" : s.kind === "merge_candidate" ? "info" : "default"}>{s.kind}</Badge>
                <span className="ml-2 font-mono">{s.name}</span>
                <span className="ml-2 text-forge-muted">— {s.evidence}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </Section>
  );
}

function ProposalRow({
  proposal,
  onChanged,
}: {
  proposal: SkillProposalSummary;
  onChanged: () => void;
}) {
  const [report, setReport] = useState<ValidationReport | null>(null);
  const [busy, setBusy] = useState(false);

  const doValidate = async () => {
    setBusy(true);
    try { setReport(await validateSkillProposal(proposal.filename)); }
    catch (e) { alert(`Validate failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  const doApprove = async () => {
    setBusy(true);
    try {
      // Backend runs validation as part of promote — if it fails we
      // surface the message instead of silently swallowing.
      const { approveSkillProposal } = await import("@/lib/ipc");
      await approveSkillProposal(proposal.filename);
      onChanged();
    } catch (e) {
      alert(`Approve failed: ${String(e)}`);
    } finally { setBusy(false); }
  };

  const doReject = async () => {
    if (!confirm(`Reject proposal "${proposal.name}"? The file will be deleted.`)) return;
    setBusy(true);
    try { await rejectSkillProposal(proposal.filename); onChanged(); }
    catch (e) { alert(`Reject failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  return (
    <div className="p-2 rounded border border-forge-border bg-forge-panel/40 space-y-1">
      <div className="flex items-center gap-2">
        <div className="flex-1 min-w-0">
          <div className="text-sm font-mono truncate">{proposal.name}</div>
          <div className="text-xs text-forge-muted truncate">
            v{proposal.version} · {proposal.description}
          </div>
        </div>
        <Button variant="ghost" onClick={doValidate} disabled={busy}>Validate</Button>
        <Button onClick={doApprove} disabled={busy}>Approve</Button>
        <Button variant="danger" onClick={doReject} disabled={busy}>Reject</Button>
      </div>
      {report && (
        <div className="text-xs space-y-0.5 mt-2">
          <div className="text-forge-muted">
            {report.ok ? "✅ passes hard checks" : "❌ hard-fail — cannot be promoted"}
          </div>
          {report.checks.map((c) => (
            <div key={c.id} className="flex items-center gap-2">
              <span className={c.passed ? "text-forge-success" : (c.severity === "hard" ? "text-forge-err" : "text-forge-warn")}>
                {c.passed ? "✓" : (c.severity === "hard" ? "✕" : "!")}
              </span>
              <span className="font-mono">{c.id}</span>
              {!c.passed && <span className="text-forge-muted">— {c.message}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function ActiveSkillRow({
  skill,
  onChanged,
}: {
  skill: SkillVersion;
  onChanged: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const versions = useQuery({
    queryKey: ["skill-versions", skill.name],
    queryFn: () => listSkillVersions(skill.name),
    enabled: expanded,
  });

  const doRollback = async (v: SkillVersion) => {
    const reason = prompt(`Rollback ${skill.name} to ${v.sha.slice(0, 8)}?\n\nOptional reason:`);
    if (reason === null) return;
    setBusy(true);
    try { await rollbackSkill(skill.name, v.sha, reason || undefined); onChanged(); }
    catch (e) { alert(`Rollback failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  const doRetire = async () => {
    const reason = prompt(`Retire ${skill.name}?\n\nRequired reason:`);
    if (!reason) return;
    setBusy(true);
    try { await retireSkill(skill.name, reason); onChanged(); }
    catch (e) { alert(`Retire failed: ${String(e)}`); }
    finally { setBusy(false); }
  };

  return (
    <div className="p-2 rounded border border-forge-border bg-forge-panel/40 space-y-1">
      <div className="flex items-center gap-2">
        <div className="flex-1 min-w-0">
          <div className="text-sm font-mono truncate">{skill.name}</div>
          <div className="text-xs text-forge-muted truncate">
            v{skill.version} · <span className="font-mono">{skill.sha.slice(0, 12)}</span> · {skill.origin}
          </div>
        </div>
        <Button variant="ghost" onClick={() => setExpanded(!expanded)}>
          {expanded ? "Hide history" : "History"}
        </Button>
        <Button variant="danger" onClick={doRetire} disabled={busy}>Retire</Button>
      </div>
      {expanded && (
        <div className="mt-2 space-y-1 text-xs">
          {versions.isLoading && <Muted>Loading history…</Muted>}
          {versions.data?.map((v) => (
            <div key={v.sha + v.promoted_at} className="flex items-center gap-2 p-1 pl-3 border-l border-forge-border">
              <span className="font-mono text-forge-accent">{v.sha.slice(0, 12)}</span>
              <Badge tone={v.origin === "rollback" ? "warn" : "info"}>{v.origin}</Badge>
              <span className="text-forge-muted">v{v.version}</span>
              <span className="text-forge-muted">· {new Date(v.promoted_at).toLocaleString()}</span>
              {v.retired_at && <Badge tone="default">retired</Badge>}
              <div className="flex-1" />
              {v.sha !== skill.sha && !v.retired_at && (
                <Button variant="ghost" onClick={() => doRollback(v)} disabled={busy}>Rollback</Button>
              )}
              {v.sha !== skill.sha && v.retired_at && (
                <Button variant="ghost" onClick={() => doRollback(v)} disabled={busy}>Restore</Button>
              )}
            </div>
          ))}
        </div>
      )}
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
