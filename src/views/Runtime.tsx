import { useCallback, useEffect, useState } from "react";
import {
  Activity,
  Check,
  Cpu,
  Download,
  FolderSync,
  HeartPulse,
  Inbox,
  RefreshCw,
  ShieldCheck,
  SlidersHorizontal,
  X,
} from "lucide-react";
import { api } from "../lib/api";
import { useApp } from "../lib/store";
import type {
  AcceleratorStatus,
  CapturePolicy,
  HealthReport,
  IntegrityReport,
  MemoryCandidate,
  ObservationSource,
  RerankerStatus,
} from "../lib/types";

type Tab = "inbox" | "health" | "models";

export function Runtime() {
  const [tab, setTab] = useState<Tab>("inbox");
  return (
    <div className="mx-auto max-w-5xl space-y-6 p-8">
      <div>
        <h2 className="font-serif text-2xl">Autonomous runtime</h2>
        <p className="mt-1 text-sm text-text-muted">
          Review captured knowledge, inspect local integrity, and control inference.
        </p>
      </div>
      <div className="flex gap-1 border-b border-border-subtle">
        <TabButton active={tab === "inbox"} onClick={() => setTab("inbox")} icon={Inbox} label="Memory inbox" />
        <TabButton active={tab === "health"} onClick={() => setTab("health")} icon={HeartPulse} label="Runtime health" />
        <TabButton active={tab === "models"} onClick={() => setTab("models")} icon={Cpu} label="Models & acceleration" />
      </div>
      {tab === "inbox" && <MemoryInbox />}
      {tab === "health" && <RuntimeHealth />}
      {tab === "models" && <Models />}
    </div>
  );
}

function TabButton({ active, onClick, icon: Icon, label }: { active: boolean; onClick: () => void; icon: typeof Inbox; label: string }) {
  return (
    <button onClick={onClick} className={`flex items-center gap-2 border-b-2 px-4 py-2 text-sm ${active ? "border-accent text-text" : "border-transparent text-text-muted hover:text-text"}`}>
      <Icon size={14} /> {label}
    </button>
  );
}

function MemoryInbox() {
  const projectId = useApp((state) => state.currentProjectId);
  const showToast = useApp((state) => state.showToast);
  const refreshMemories = useApp((state) => state.refreshMemories);
  const [candidates, setCandidates] = useState<MemoryCandidate[]>([]);
  const [sources, setSources] = useState<ObservationSource[]>([]);
  const [policy, setPolicy] = useState<CapturePolicy | null>(null);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
  const [sourceKind, setSourceKind] = useState<"codex" | "claude_code">("codex");
  const [sourcePath, setSourcePath] = useState("");
  const [candidateSource, setCandidateSource] = useState("all");
  const [candidateCategory, setCandidateCategory] = useState("all");
  const [mergeTargets, setMergeTargets] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [nextCandidates, nextSources, nextPolicy] = await Promise.all([
        api.listMemoryCandidates(projectId),
        api.listObservationSources(projectId),
        api.getCapturePolicy(projectId),
      ]);
      setCandidates(nextCandidates);
      setSources(nextSources);
      setPolicy(nextPolicy);
    } catch (error) {
      showToast({ kind: "err", text: String(error) });
    } finally {
      setLoading(false);
    }
  }, [projectId, showToast]);

  useEffect(() => { void load(); }, [load]);

  async function decide(candidate: MemoryCandidate, action: "approve" | "edit_and_approve" | "reject" | "merge" | "defer") {
    try {
      await api.reviewMemoryCandidate({
        candidate_id: candidate.id,
        action,
        edited_content: action === "edit_and_approve" ? editText : null,
        target_memory_id: action === "merge" ? mergeTargets[candidate.id]?.trim() : null,
        expected_version: candidate.version,
        decided_by: "desktop",
      });
      setCandidates((items) => items.filter((item) => item.id !== candidate.id));
      setEditing(null);
      if (action === "approve" || action === "edit_and_approve") await refreshMemories();
      showToast({ kind: "ok", text: action === "reject" ? "Candidate rejected" : action === "defer" ? "Candidate deferred" : action === "merge" ? "Candidate merged" : "Memory activated" });
    } catch (error) {
      showToast({ kind: "err", text: String(error) });
      await load();
    }
  }

  async function bulk(action: "approve" | "reject") {
    const selected = filteredCandidates.filter((candidate) => action === "reject" || (!candidate.contradiction_uid && !candidate.duplicate_memory_uid));
    try {
      for (const candidate of selected) {
        await api.reviewMemoryCandidate({ candidate_id: candidate.id, action, expected_version: candidate.version, decided_by: "desktop-bulk" });
      }
      if (action === "approve") await refreshMemories();
      await load();
      showToast({ kind: "ok", text: `${selected.length} candidate(s) ${action === "approve" ? "approved" : "rejected"}` });
    } catch (error) {
      showToast({ kind: "err", text: String(error) });
      await load();
    }
  }

  async function addSource() {
    if (!sourcePath.trim() || !policy) return;
    try {
      if (!policy.enabled_sources.includes(sourceKind)) {
        await api.updateCapturePolicy({ ...policy, enabled_sources: [...policy.enabled_sources, sourceKind] });
      }
      await api.configureObservationSource({
        id: "",
        project_id: projectId,
        kind: sourceKind,
        name: sourceKind === "codex" ? "Codex transcripts" : "Claude Code transcripts",
        root_path: sourcePath.trim(),
        enabled: true,
        config: {},
        last_sync_at: null,
        last_error: null,
        processed_count: 0,
        candidate_count: 0,
        created_at: 0,
        updated_at: 0,
      });
      setSourcePath("");
      await load();
      showToast({ kind: "ok", text: "Local transcript source enabled" });
    } catch (error) {
      showToast({ kind: "err", text: String(error) });
    }
  }

  async function sync(source: ObservationSource) {
    try {
      const operation = await api.startSourceSync(source.id);
      showToast({ kind: "info", text: `Source sync started: ${operation.id}` });
    } catch (error) {
      showToast({ kind: "err", text: String(error) });
    }
  }

  const filteredCandidates = candidates.filter((candidate) =>
    (candidateSource === "all" || candidate.source_kind === candidateSource)
    && (candidateCategory === "all" || candidate.mem_type === candidateCategory));
  const safeBulkCount = filteredCandidates.filter((candidate) => !candidate.contradiction_uid && !candidate.duplicate_memory_uid).length;

  return (
    <div className="space-y-5">
      <section className="card p-5">
        <div className="flex items-center gap-2"><FolderSync size={16} className="text-accent" /><h3 className="font-medium">Local transcript sources</h3></div>
        <p className="mt-1 text-xs text-text-muted">Opt-in and read-only. Complete transcripts are never copied into biTurbo.</p>
        <div className="mt-4 flex gap-2">
          <select className="input max-w-44" value={sourceKind} onChange={(event) => setSourceKind(event.target.value as "codex" | "claude_code")}>
            <option value="codex">Codex</option><option value="claude_code">Claude Code</option>
          </select>
          <input className="input" value={sourcePath} onChange={(event) => setSourcePath(event.target.value)} placeholder="Absolute transcript directory" />
          <button className="btn-primary" onClick={() => void addSource()} disabled={!sourcePath.trim()}>Enable</button>
        </div>
        {sources.length > 0 && <div className="mt-4 divide-y divide-border-subtle">{sources.map((source) => (
          <div key={source.id} className="flex items-center gap-3 py-3 text-sm">
            <div className="min-w-0 flex-1"><div className="font-medium">{source.name}</div><div className="truncate text-xs text-text-dim">{source.root_path}</div>{source.last_error && <div className="mt-1 text-xs text-danger">{source.last_error}</div>}</div>
            <span className="text-xs text-text-muted">{source.candidate_count} candidates</span>
            <button className="btn-outline" onClick={() => void sync(source)}><RefreshCw size={12} /> Sync</button>
          </div>
        ))}</div>}
      </section>

      <div className="flex flex-wrap items-end justify-between gap-3"><div><h3 className="font-medium">Pending candidates</h3><p className="text-xs text-text-muted">Candidates are excluded from recall until approved.</p></div><div className="flex flex-wrap gap-2"><select className="input max-w-36" value={candidateSource} onChange={(event) => setCandidateSource(event.target.value)}><option value="all">All sources</option><option value="generic">Generic API</option><option value="codex">Codex</option><option value="claude_code">Claude Code</option></select><select className="input max-w-36" value={candidateCategory} onChange={(event) => setCandidateCategory(event.target.value)}><option value="all">All categories</option>{[...new Set(candidates.map((candidate) => candidate.mem_type))].map((category) => <option key={category} value={category}>{category}</option>)}</select><button className="btn-outline" disabled={safeBulkCount === 0} onClick={() => void bulk("approve")}><Check size={13} /> Approve safe ({safeBulkCount})</button><button className="btn-ghost text-danger" disabled={filteredCandidates.length === 0} onClick={() => void bulk("reject")}><X size={13} /> Reject shown</button><button className="btn-ghost" onClick={() => void load()}><RefreshCw size={13} /> Refresh</button></div></div>
      {loading ? <div className="py-12 text-center text-sm text-text-muted">Loading candidates…</div> : filteredCandidates.length === 0 ? <div className="card py-12 text-center text-sm text-text-muted"><ShieldCheck className="mx-auto mb-3 text-success" size={24} />Inbox is clear.</div> : <div className="space-y-3">{filteredCandidates.map((candidate) => (
        <article key={candidate.id} className="card p-5">
          <div className="flex items-start gap-4"><div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2"><span className="chip">{candidate.mem_type}</span><span className="chip">{candidate.source_kind}</span><span className="text-xs text-text-dim">{Math.round(candidate.confidence * 100)}% confidence · {new Date(candidate.source_timestamp).toLocaleString()}</span>{candidate.contradiction_uid && <span className="rounded bg-danger/10 px-2 py-0.5 text-xs text-danger">contradiction</span>}{candidate.duplicate_memory_uid && <span className="rounded bg-warning/10 px-2 py-0.5 text-xs text-warning">duplicate</span>}</div>
            {editing === candidate.id ? <textarea className="input mt-3 min-h-24" value={editText} onChange={(event) => setEditText(event.target.value)} /> : <p className="mt-3 text-sm leading-relaxed">{candidate.content}</p>}
            {candidate.evidence[0] && <blockquote className="mt-3 border-l-2 border-border pl-3 text-xs text-text-muted">{candidate.evidence[0].excerpt}</blockquote>}
            {(candidate.contradiction_uid || candidate.duplicate_memory_uid) && <div className="mt-2 font-mono text-[10px] text-text-dim">Related memory: {candidate.contradiction_uid ?? candidate.duplicate_memory_uid}</div>}
          </div></div>
          <div className="mt-4 flex flex-wrap justify-end gap-2">
            <button className="btn-ghost" onClick={() => void decide(candidate, "defer")}>Defer</button><button className="btn-ghost text-danger" onClick={() => void decide(candidate, "reject")}><X size={13} /> Reject</button>
            <input className="input max-w-48" value={mergeTargets[candidate.id] ?? ""} onChange={(event) => setMergeTargets((targets) => ({ ...targets, [candidate.id]: event.target.value }))} placeholder="Existing memory ID" /><button className="btn-outline" disabled={!mergeTargets[candidate.id]?.trim()} onClick={() => void decide(candidate, "merge")}>Merge</button>
            {editing === candidate.id ? <><button className="btn-ghost" onClick={() => setEditing(null)}>Cancel edit</button><button className="btn-primary" onClick={() => void decide(candidate, "edit_and_approve")} disabled={!editText.trim()}><Check size={13} /> Save & approve</button></> : <><button className="btn-outline" onClick={() => { setEditing(candidate.id); setEditText(candidate.content); }}>Edit</button><button className="btn-primary" onClick={() => void decide(candidate, "approve")}><Check size={13} /> Approve</button></>}
          </div>
        </article>
      ))}</div>}
    </div>
  );
}

function RuntimeHealth() {
  const projectId = useApp((state) => state.currentProjectId);
  const showToast = useApp((state) => state.showToast);
  const [health, setHealth] = useState<HealthReport | null>(null);
  const [report, setReport] = useState<IntegrityReport | null>(null);
  const [running, setRunning] = useState(false);
  const load = useCallback(async () => {
    try { const next = await api.healthReport(); setHealth(next); setReport(next.last_integrity_run); } catch (error) { showToast({ kind: "err", text: String(error) }); }
  }, [showToast]);
  useEffect(() => { void load(); }, [load]);

  async function audit() {
    setRunning(true);
    try {
      const operation = await api.startIntegrityCheck(projectId);
      for (;;) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        const current = await api.operationStatus(operation.id);
        if (current.status === "succeeded") break;
        if (current.status === "failed" || current.status === "cancelled") throw new Error(current.error ?? current.status);
      }
      const latest = await api.integrityReport(null, projectId);
      setReport(latest); await load();
    } catch (error) { showToast({ kind: "err", text: String(error) }); } finally { setRunning(false); }
  }

  async function repair(issueIds: string[]) {
    if (!report) return;
    try { await api.repairIntegrity({ project_id: projectId, integrity_run_id: report.id, issue_ids: issueIds, dry_run: false }); showToast({ kind: "info", text: "Safe repair started" }); }
    catch (error) { showToast({ kind: "err", text: String(error) }); }
  }

  return <div className="space-y-5">
    <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
      <Metric label="Status" value={health?.status ?? "checking"} /><Metric label="Pending journal" value={health?.pending_mutations ?? 0} /><Metric label="Candidates" value={health?.pending_candidates ?? 0} /><Metric label="Recoverable ops" value={health?.recoverable_operations ?? 0} />
    </div>
    <section className="card p-5"><div className="flex items-center justify-between"><div><div className="flex items-center gap-2"><Activity size={16} className="text-accent" /><h3 className="font-medium">Integrity audit</h3></div><p className="mt-1 text-xs text-text-muted">Checks SQLite, FTS, vectors, journals, counters, candidates, and runtime leases.</p></div><button className="btn-primary" disabled={running} onClick={() => void audit()}><RefreshCw size={13} />{running ? "Auditing…" : "Run audit"}</button></div></section>
    {report && <section className="card overflow-hidden"><div className="flex items-center justify-between border-b border-border-subtle px-5 py-4"><div><h3 className="font-medium">Latest report</h3><p className="text-xs text-text-dim">{report.id} · {report.issue_count} issue(s)</p></div>{report.issues.some((issue) => issue.safe_automatic && !issue.repaired_at) && <button className="btn-outline" onClick={() => void repair(report.issues.filter((issue) => issue.safe_automatic && !issue.repaired_at).map((issue) => issue.id))}><ShieldCheck size={13} /> Repair safe issues</button>}</div><div className="divide-y divide-border-subtle">{report.issues.length === 0 ? <div className="p-8 text-center text-sm text-success">All checked invariants agree.</div> : report.issues.map((issue) => <div key={issue.id} className="flex items-start gap-3 px-5 py-4"><SlidersHorizontal size={14} className="mt-0.5 text-text-dim" /><div className="flex-1"><div className="text-sm font-medium">{issue.issue_kind}</div><div className="mt-1 text-xs text-text-muted">{issue.recommended_action}</div><div className="mt-1 font-mono text-[10px] text-text-dim">expected {issue.expected_state} · actual {issue.actual_state}</div></div><span className="chip">{issue.repaired_at ? "repaired" : issue.safe_automatic ? "safe" : "review"}</span></div>)}</div></section>}
  </div>;
}

function Models() {
  const showToast = useApp((state) => state.showToast);
  const [accelerator, setAccelerator] = useState<AcceleratorStatus | null>(null);
  const [reranker, setReranker] = useState<RerankerStatus | null>(null);
  const load = useCallback(async () => { try { const [a, r] = await Promise.all([api.acceleratorStatus(), api.rerankerStatus()]); setAccelerator(a); setReranker(r); } catch (error) { showToast({ kind: "err", text: String(error) }); } }, [showToast]);
  useEffect(() => { void load(); }, [load]);
  async function setProvider(provider: "auto" | "cpu" | "cuda") { try { await api.setAcceleratorPreference(provider); await load(); showToast({ kind: "ok", text: `Embedding provider set to ${provider}` }); } catch (error) { showToast({ kind: "err", text: String(error) }); } }
  async function download() { try { const operation = await api.startRerankerDownload(); showToast({ kind: "info", text: `Model download started: ${operation.id}` }); } catch (error) { showToast({ kind: "err", text: String(error) }); } }
  async function toggleReranker() { if (!reranker) return; try { setReranker(await api.setRerankerEnabled(!reranker.enabled)); } catch (error) { showToast({ kind: "err", text: String(error) }); } }
  return <div className="space-y-5">
    <section className="card p-5"><div className="flex items-center gap-2"><Cpu size={16} className="text-accent" /><h3 className="font-medium">Embedding execution provider</h3></div><p className="mt-1 text-xs text-text-muted">The environment override takes precedence. Forced CUDA fails clearly when unavailable.</p><div className="mt-4 flex gap-2">{(["auto", "cpu", "cuda"] as const).map((provider) => <button key={provider} className={accelerator?.requested_provider === provider ? "btn-primary" : "btn-outline"} disabled={provider === "cuda" && !accelerator?.compiled_providers.includes("cuda")} onClick={() => void setProvider(provider)}>{provider.toUpperCase()}</button>)}</div>{accelerator && <div className="mt-5 grid grid-cols-2 gap-3 text-sm md:grid-cols-4"><Metric label="Effective" value={accelerator.effective_provider} /><Metric label="CUDA available" value={accelerator.cuda_available ? "yes" : "no"} /><Metric label="Fallbacks" value={accelerator.fallback_count} /><Metric label="Avg. batch" value={`${accelerator.average_batch_ms.toFixed(1)} ms`} /></div>}{accelerator?.initialization_error && <p className="mt-3 text-xs text-danger">{accelerator.initialization_error}</p>}</section>
    <section className="card p-5"><div className="flex items-start justify-between gap-4"><div><div className="flex items-center gap-2"><SlidersHorizontal size={16} className="text-accent" /><h3 className="font-medium">Cross-encoder reranker</h3></div><p className="mt-1 text-xs text-text-muted">Pinned local ONNX model. The 91 MB download is explicit and verified by SHA-256.</p></div>{reranker?.installed ? <button className={reranker.enabled ? "btn-primary" : "btn-outline"} onClick={() => void toggleReranker()}>{reranker.enabled ? "Enabled" : "Enable"}</button> : <button className="btn-primary" onClick={() => void download()}><Download size={13} /> Download</button>}</div>{reranker && <div className="mt-4 text-xs text-text-muted"><div>{reranker.model_name}</div><div className="mt-1 font-mono text-[10px] text-text-dim">{reranker.version} · {reranker.license}</div>{reranker.last_error && <div className="mt-2 text-danger">{reranker.last_error}</div>}</div>}</section>
  </div>;
}

function Metric({ label, value }: { label: string; value: string | number }) {
  return <div className="rounded-md border border-border-subtle bg-surface-2 p-3"><div className="text-[10px] uppercase tracking-wider text-text-dim">{label}</div><div className="mt-1 font-mono text-sm text-text">{value}</div></div>;
}
