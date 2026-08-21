import { useEffect, useRef, useState } from "react";
import { useApp, useConfirm } from "../lib/store";
import { api } from "../lib/api";
import { open, save } from "@tauri-apps/plugin-dialog";
import { Plus, FolderGit2, Trash2, Database, FileSearch, Loader2, Eye, Download, FileText, Radar, FilePlus2 } from "lucide-react";
import clsx from "clsx";
import type { IngestProgress } from "../lib/types";
import { friendlyError } from "../lib/format";

export function Projects() {
  const projects = useApp((s) => s.projects);
  const refreshProjects = useApp((s) => s.refreshProjects);
  const refreshStats = useApp((s) => s.refreshStats);
  const showToast = useApp((s) => s.showToast);
  const setCurrentProjectId = useApp((s) => s.setCurrentProjectId);
  const currentProjectId = useApp((s) => s.currentProjectId);
  const confirm = useConfirm();

  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [desc, setDesc] = useState("");
  const [rootPath, setRootPath] = useState("");
  const ingestJobs = useApp((s) => s.ingestJobs);
  const ingestJobIds = useApp((s) => s.ingestJobIds);
  const registerIngestJob = useApp((s) => s.registerIngestJob);
  const [busy, setBusy] = useState<string | null>(null);
  const [watchOn, setWatchOn] = useState<Record<string, boolean>>({});
  const [importingFor, setImportingFor] = useState<string | null>(null);

  useEffect(() => {
    const next: Record<string, boolean> = {};
    for (const p of projects) next[p.id] = p.watch_enabled;
    setWatchOn(next);
  }, [projects]);

  const activeIngest = Object.values(ingestJobs)[0] as IngestProgress | undefined;

  async function pickFolder() {
    const sel = await open({ directory: true, multiple: false });
    if (typeof sel === "string") setRootPath(sel);
  }

  async function create() {
    const trimmed = name.trim();
    if (!trimmed) return;
    if (projects.some((p) => p.name.toLowerCase() === trimmed.toLowerCase())) {
      setNameError("A project with this name already exists");
      return;
    }
    setNameError(null);
    setBusy("create");
    try {
      const p = await api.createProject({
        name: name.trim(),
        description: desc.trim() || undefined,
        root_path: rootPath.trim() || undefined,
      });
      await refreshProjects();
      await refreshStats();
      setCreating(false);
      setName("");
      setDesc("");
      setRootPath("");
      setCurrentProjectId(p.id);
      showToast({ kind: "ok", text: `Created project ${p.name}` });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setBusy(null);
    }
  }

  async function ingest(projectId: string, root: string) {
    if (!root) {
      showToast({ kind: "err", text: "Set a root_path first" });
      return;
    }
    try {
      const job = await api.ingestProject(projectId, root);
      registerIngestJob(projectId, job.job_id);
      showToast({ kind: "info", text: `Started indexing ${projectId}…` });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    }
  }

  async function cancelIngest() {
    if (!activeIngest) return;
    const jobId = ingestJobIds[activeIngest.project_id];
    if (!jobId) {
      showToast({ kind: "err", text: "Cannot cancel — job id unknown" });
      return;
    }
    try {
      await api.cancelOperation(jobId);
      showToast({ kind: "info", text: "Cancellation requested" });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    }
  }

  async function remove(id: string, name: string) {
    const ok = await confirm({
      title: `Delete project "${name}"?`,
      body: (
        <>
          All memories and the code index for <b>{name}</b> will be
          permanently removed. This cannot be undone.
        </>
      ),
      confirmLabel: "Delete project",
    });
    if (!ok) return;
    setBusy(id);
    try {
      await api.deleteProject(id);
      await refreshProjects();
      await refreshStats();
      showToast({ kind: "ok", text: "Deleted" });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setBusy(null);
    }
  }

  async function importFolder(projectId: string) {
    const sel = await open({ directory: true, multiple: false, title: `Import folder into ${projectId}` });
    if (typeof sel !== "string") return;
    setImportingFor(projectId);
    try {
      const r = await api.importFolder(projectId, sel);
      await refreshProjects();
      await refreshStats();
      showToast({
        kind: "ok",
        text: `Imported ${r.files_imported} files · ${r.memories_created} memories${r.errors.length ? ` · ${r.errors.length} errors` : ""}`,
      });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setImportingFor(null);
    }
  }

  async function exportProject(projectId: string | null) {
    const suggested = `biturbo-${projectId ?? "all"}-${Date.now()}.json`;
    const target = await save({
      defaultPath: suggested,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!target) return;
    try {
      const r = await api.exportMemories(projectId, target);
      showToast({ kind: "ok", text: `Exported ${r.memories_written} memories → ${r.output_path}` });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    }
  }

  async function toggleWatch(projectId: string, root: string | null, enabled: boolean) {
    try {
      await api.setWatch(projectId, root, enabled);
      setWatchOn((s) => ({ ...s, [projectId]: enabled }));
      showToast({
        kind: "ok",
        text: enabled ? `Watching ${projectId} (auto-reingest on changes)` : `Stopped watching ${projectId}`,
      });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    }
  }

  const [repairing, setRepairing] = useState<string | null>(null);

  async function repairMarkerFiles(projectId: string) {
    setRepairing(projectId);
    try {
      const r = await api.ensureProjectMarkerFiles(projectId);
      showToast({
        kind: "ok",
        text: r.created.length ? `Created ${r.created.join(", ")}` : "Already up to date",
      });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setRepairing(null);
    }
  }

  const [modelEdit, setModelEdit] = useState<{
    id: string;
    name: string;
    current: string | null;
  } | null>(null);
  const [modelSaving, setModelSaving] = useState(false);

  async function saveModel(model: string | null) {
    if (!modelEdit) return;
    setModelSaving(true);
    try {
      await api.setProjectEmbedModel(modelEdit.id, model);
      await refreshProjects();
      showToast({
        kind: "ok",
        text: model ? `Set model to ${model}` : "Cleared model preference",
      });
      setModelEdit(null);
    } catch (e) {
conflict://2
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setModelSaving(false);
    }
  }

  return (
    <div className="mx-auto max-w-5xl space-y-6 p-8 animate-fade_in">
      <div className="flex items-baseline justify-between">
        <div>
          <h2 className="font-serif text-2xl">Projects</h2>
          <p className="mt-1 text-sm text-text-muted">
            Each project gets its own turbovec index, isolated memories, and a tree-sitter code map.
          </p>
        </div>
        <button onClick={() => setCreating(true)} className="btn-primary">
          <Plus size={14} /> New project
        </button>
      </div>

      {projects.length === 0 && !creating && (
        <div className="card flex flex-col items-center justify-center p-12 text-center">
          <FolderGit2 size={28} className="mb-3 text-text-dim" />
          <div className="font-serif text-lg">No projects yet</div>
          <p className="mt-1 max-w-md text-sm text-text-muted">
            Projects isolate memories and the code index per repository. Create your first
            project, point it at a repo, and run Re-index — then connect an agent via
            Settings → MCP.
          </p>
          <button onClick={() => setCreating(true)} className="btn-primary mt-4">
            <Plus size={14} /> Create your first project
          </button>
        </div>
      )}

      {activeIngest && (
        <div className="card p-4">
          <div className="mb-2 flex items-center gap-2 text-sm">
            <Loader2 size={14} className="animate-spin text-accent" />
            <span className="font-medium text-text">
              {activeIngest.phase === "scanning" && "Scanning project…"}
              {activeIngest.phase === "parsing" && "Parsing files…"}
              {activeIngest.phase === "embedding" && "Embedding chunks…"}
              {activeIngest.phase === "writing" && "Writing chunks…"}
              {activeIngest.phase === "edges" && "Building edges…"}
              {activeIngest.phase === "done" && "Done"}
            </span>
            {activeIngest.total > 0 && activeIngest.phase !== "done" && (
              <span className="ml-auto font-mono text-xs text-text-muted">
                {activeIngest.current}/{activeIngest.total} · {activeIngest.chunks_so_far} chunks
              </span>
            )}
            {activeIngest.phase !== "done" && (
              <button
                onClick={() => void cancelIngest()}
                className="btn-ghost ml-auto shrink-0 px-2 py-0.5 text-[11px]"
                title="Request cancellation of this indexing run"
              >
                Cancel
              </button>
            )}
            {activeIngest.phase === "done" && (
              <span className="ml-auto font-mono text-xs text-success">
                {activeIngest.chunks_so_far} chunks indexed
              </span>
            )}
          </div>
          {activeIngest.total > 0 && activeIngest.phase !== "done" && (
            <div className="h-1.5 overflow-hidden rounded-full bg-surface-2">
              <div
                className="h-full bg-accent transition-all"
                style={{ width: `${Math.min(100, (activeIngest.current / activeIngest.total) * 100)}%` }}
              />
            </div>
          )}
          {activeIngest.file && (
            <div className="mt-2 truncate font-mono text-[11px] text-text-dim">
              {activeIngest.file}
            </div>
          )}
        </div>
      )}

      {creating && (
        <form
          className="card space-y-3 p-5"
          onSubmit={(e) => {
            e.preventDefault();
            void create();
          }}
        >
          <h3 className="font-serif text-lg">New project</h3>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-[10px] uppercase tracking-widest text-text-dim">
                Name
              </label>
              <input
                value={name}
                onChange={(e) => {
                  setName(e.target.value);
                  setNameError(null);
                }}
                placeholder="scout-qa"
                className="input"
                autoFocus
              />
              {nameError && (
                <p role="alert" className="mt-1 text-xs text-danger">
                  {nameError}
                </p>
              )}
            </div>
            <div>
              <label className="mb-1 block text-[10px] uppercase tracking-widest text-text-dim">
                Description
              </label>
              <input
                value={desc}
                onChange={(e) => setDesc(e.target.value)}
                placeholder="Laravel rewrite of QA studio"
                className="input"
              />
            </div>
          </div>
          <div>
            <label className="mb-1 block text-[10px] uppercase tracking-widest text-text-dim">
              Root path (for code indexing)
            </label>
            <div className="flex gap-2">
              <input
                value={rootPath}
                onChange={(e) => setRootPath(e.target.value)}
                placeholder="/Users/you/Code/project"
                className="input font-mono"
              />
              <button type="button" onClick={pickFolder} className="btn-outline">
                Browse
              </button>
            </div>
          </div>
          <div className="flex items-center justify-end gap-2 border-t border-border-subtle pt-3">
            <button type="button" onClick={() => setCreating(false)} className="btn-ghost">
              Cancel
            </button>
            <button
              type="submit"
              disabled={!name.trim() || busy === "create"}
              className="btn-primary"
            >
              {busy === "create" ? "Creating…" : "Create"}
            </button>
          </div>
        </form>
      )}

      <div className="grid gap-3">
        {projects.map((p) => {
          const active = p.id === currentProjectId;
          const isIngesting = !!ingestJobs[p.id];
          return (
            <div
              key={p.id}
              className={clsx(
                "card p-5 transition",
                active && "border-accent/40 bg-accent-soft/40"
              )}
            >
              <div className="flex items-start gap-4">
                <div
                  className={clsx(
                    "flex h-10 w-10 shrink-0 items-center justify-center rounded-md",
                    active ? "bg-accent/20 text-accent" : "bg-surface-2 text-text-muted"
                  )}
                >
                  <FolderGit2 size={18} />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex items-baseline gap-2">
                    <h3 className="font-serif text-lg text-text">{p.name}</h3>
                    {active && (
                      <span className="rounded-full border border-accent/30 bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent">
                        active
                      </span>
                    )}
                    <span className="ml-auto font-mono text-[10px] text-text-dim">
                      {p.id}
                    </span>
                  </div>
                  {p.description && (
                    <p className="mt-0.5 text-sm text-text-muted">{p.description}</p>
                  )}
                  {p.root_path && (
                    <p className="mt-1 font-mono text-[11px] text-text-dim">
                      {p.root_path}
                    </p>
                  )}

                  <div className="mt-3 flex items-center gap-4 text-xs text-text-muted">
                    <span className="inline-flex items-center gap-1.5">
                      <Database size={11} />
                      <span className="font-mono">{p.memory_count}</span> memories
                    </span>
                    <span className="inline-flex items-center gap-1.5">
                      <FileSearch size={11} />
                      <span className="font-mono">{p.indexed_count}</span> code chunks
                    </span>
                    <span className="font-mono text-[10px] text-text-dim">
                      dim={p.dim} · {p.bit_width}-bit{p.embed_model ? ` · ${p.embed_model}` : ""}
                    </span>
                    {watchOn[p.id] && (
                      <span
                        className="inline-flex items-center gap-1 rounded-full border border-success/30 bg-success/10 px-1.5 py-0.5 text-[10px] text-success"
                        title="Watching for changes"
                      >
                        <Radar size={9} /> watching
                      </span>
                    )}
                  </div>
                </div>
              </div>

              <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-border-subtle pt-3">
                {!active && (
                  <button
                    onClick={() => setCurrentProjectId(p.id)}
                    className="btn-outline"
                  >
                    Switch to this
                  </button>
                )}
                {p.root_path && (
                  <button
                    onClick={() => ingest(p.id, p.root_path!)}
                    disabled={isIngesting}
                    className="btn-outline"
                  >
                    <FileSearch size={12} />
                    {isIngesting ? "Indexing…" : "Re-index code"}
                  </button>
                )}
                <button
                  onClick={() => importFolder(p.id)}
                  disabled={importingFor === p.id}
                  className="btn-outline"
                  title="Import all .md/.txt files in a folder as memories"
                >
                  <FileText size={12} />
                  {importingFor === p.id ? "Importing…" : "Import .md folder"}
                </button>
                <button
                  onClick={() => exportProject(p.id)}
                  className="btn-outline"
                  title="Export all memories of this project to JSON"
                >
                  <Download size={12} /> Export
                </button>
                {p.root_path && (
                  <button
                    onClick={() => repairMarkerFiles(p.id)}
                    disabled={repairing === p.id}
                    className="btn-outline"
                    title="Generate .biTurbo / .biturboignore if missing (legacy projects)"
                  >
                    <FilePlus2 size={12} />
                    {repairing === p.id ? "Generating…" : "Generate marker files"}
                  </button>
                )}
                {p.root_path && (
                  <button
                    onClick={() => toggleWatch(p.id, p.root_path, !watchOn[p.id])}
                    className={clsx(
                      "btn-outline",
                      watchOn[p.id] && "border-success/40 text-success"
                    )}
                    title={watchOn[p.id] ? "Stop watching for changes" : "Watch for changes; auto-reingest on file events"}
                  >
                    <Radar size={12} />
                    {watchOn[p.id] ? "Unwatch" : "Watch"}
                  </button>
                )}
                <button
                  onClick={() =>
                    setModelEdit({ id: p.id, name: p.name, current: p.embed_model })
                  }
                  className="btn-outline"
                  title="Set preferred embedding model for this project"
                >
                  embed model
                </button>
                <div className="flex-1" />
                <button
                  onClick={() => exportProject(null)}
                  className="btn-ghost text-[11px] text-text-muted"
                  title="Export all memories across all projects"
                >
                  <Eye size={11} /> Export all
                </button>
                {p.id !== "default" && (
                  <button
                    onClick={() => remove(p.id, p.name)}
                    disabled={busy === p.id}
                    className="btn-ghost text-danger hover:bg-danger/10"
                    title="Delete project"
                  >
                    <Trash2 size={12} />
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
      {modelEdit && (
        <EmbedModelModal
          projectId={modelEdit.name}
          current={modelEdit.current}
          saving={modelSaving}
          onSave={saveModel}
          onClose={() => setModelEdit(null)}
        />
      )}
    </div>
  );
}

const EMBED_MODELS = [
  { id: "BGE-small-en-v1.5", hint: "384 dims · fast, low memory" },
  { id: "BGE-base-en-v1.5", hint: "768 dims · balanced" },
  { id: "BGE-large-en-v1.5", hint: "1024 dims · highest quality" },
  { id: "all-MiniLM-L6-v2", hint: "384 dims · fast" },
] as const;

function EmbedModelModal({
  projectId,
  current,
  saving,
  onSave,
  onClose,
}: {
  projectId: string;
  current: string | null;
  saving: boolean;
  onSave: (model: string | null) => void;
  onClose: () => void;
}) {
  const [selected, setSelected] = useState<string | null>(current);
  const panelRef = useRef<HTMLDivElement>(null);

  // Move focus into the dialog so Escape and screen readers catch it.
  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-bg/70 p-4 backdrop-blur-sm animate-backdrop_in"
      onClick={() => {
        if (!saving) onClose();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !saving) onClose();
      }}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="embed-model-title"
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-md rounded-lg border border-border bg-surface shadow-modal outline-none animate-modal_in"
      >
        <div className="border-b border-border-subtle p-5">
          <h3 id="embed-model-title" className="font-serif text-lg text-text">
            Embedding model
          </h3>
          <p className="mt-1 text-xs text-text-muted">
            Model used to embed new memories and code chunks for{" "}
            <span className="font-medium text-text">{projectId}</span>. Changing it applies to
            the next index run.
          </p>
        </div>

        <div className="space-y-1 p-3" role="radiogroup" aria-label="Embedding model">
          <label className="flex cursor-pointer items-start gap-2.5 rounded-md px-2 py-1.5 hover:bg-surface-2">
            <input
              type="radio"
              name="embed-model"
              checked={selected === null}
              onChange={() => setSelected(null)}
              className="mt-0.5 accent-accent"
            />
            <span>
              <span className="block text-sm text-text">Project default</span>
              <span className="block text-[11px] text-text-dim">
                Clear the override — use the app-wide default
              </span>
            </span>
          </label>
          {EMBED_MODELS.map((m) => (
            <label
              key={m.id}
              className="flex cursor-pointer items-start gap-2.5 rounded-md px-2 py-1.5 hover:bg-surface-2"
            >
              <input
                type="radio"
                name="embed-model"
                checked={selected === m.id}
                onChange={() => setSelected(m.id)}
                className="mt-0.5 accent-accent"
              />
              <span>
                <span className="block font-mono text-sm text-text">{m.id}</span>
                <span className="block text-[11px] text-text-dim">{m.hint}</span>
              </span>
            </label>
          ))}
        </div>

        <div className="flex items-center justify-end gap-2 border-t border-border-subtle px-5 py-3">
          <button onClick={onClose} disabled={saving} className="btn-outline">
            Cancel
          </button>
          <button
            onClick={() => onSave(selected)}
            disabled={saving || selected === current}
            className="btn-primary"
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>
  );
}
