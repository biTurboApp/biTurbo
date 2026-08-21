import { Plus, Sparkles, Sun, Moon, Loader2 } from "lucide-react";
import { useApp } from "../lib/store";
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
import { ingestPhaseLabel } from "../lib/format";
import type { ConsolidateReport } from "../lib/types";

export function TopBar() {
  const setQuickAddOpen = useApp((s) => s.setQuickAddOpen);
  const view = useApp((s) => s.view);
  const stats = useApp((s) => s.stats);
  const currentProjectId = useApp((s) => s.currentProjectId);
  const projects = useApp((s) => s.projects);
  const refreshStats = useApp((s) => s.refreshStats);
  const refreshActivity = useApp((s) => s.refreshActivity);
  const showToast = useApp((s) => s.showToast);
  const theme = useApp((s) => s.theme);
  const toggleTheme = useApp((s) => s.toggleTheme);
  const [consolidating, setConsolidating] = useState(false);

  const currentProject = projects.find((p) => p.id === currentProjectId);
  const ingestJobs = useApp((s) => s.ingestJobs);
  const activeIngests = Object.values(ingestJobs).filter(
    (j) => j.phase !== "done"
  );

  useEffect(() => {
    const unlistenP = listen<ConsolidateReport>("consolidate:done", (e) => {
      const r = e.payload;
      setConsolidating(false);
      showToast({
        kind: "ok",
        text: `Consolidated · ${r.decayed} decayed · ${r.merged} merged · ${r.duplicates_found} dupes`,
      });
      void refreshStats();
      void refreshActivity();
    });
    return () => {
      void unlistenP.then((fn) => fn());
    };
  }, [showToast, refreshStats, refreshActivity]);

  async function runConsolidate() {
    setConsolidating(true);
    try {
      await api.consolidate(currentProjectId);
    } catch (e) {
      setConsolidating(false);
      showToast({ kind: "err", text: String(e) });
    }
  }

  return (
    <header
      data-tauri-drag-region
      className="flex h-14 shrink-0 items-center gap-3 border-b border-border-subtle bg-bg/40 px-4 backdrop-blur"
    >
      {/* View title */}
      <div className="flex items-baseline gap-3 pl-16">
        <h1 className="font-serif text-lg font-medium capitalize text-text">
          {view}
        </h1>
        {currentProject && view !== "projects" && view !== "settings" && (
          <span className="font-mono text-[11px] text-text-dim">
            {currentProject.name}
          </span>
        )}
      </div>

      <div className="flex-1" />

      {/* Ingest progress */}
      {activeIngests.length > 0 && (
        <div className="hidden items-center gap-2 md:flex">
          <Loader2 size={12} className="animate-spin text-accent" />
          <span className="text-[11px] text-text-muted">
            {ingestPhaseLabel(activeIngests[0].phase)}…
          </span>
          {activeIngests[0].total > 0 && (
            <div className="h-1 w-16 overflow-hidden rounded-full bg-surface-2">
              <div
                className="h-full bg-accent transition-all"
                style={{
                  width: `${Math.min(100, (activeIngests[0].current / activeIngests[0].total) * 100)}%`,
                }}
              />
            </div>
          )}
        </div>
      )}

      {/* Index size badge */}
      {stats && (
        <div className="hidden items-center gap-2 rounded-md border border-border-subtle bg-surface px-2.5 py-1 text-[11px] text-text-muted md:flex">
          <span className="font-mono">
            {(stats.index_bytes / 1024 / 1024).toFixed(2)} MB
          </span>
          <span className="text-text-dim">·</span>
          <span>
            {stats.total_memories.toLocaleString()} memories
          </span>
        </div>
      )}

      <button
        onClick={runConsolidate}
        disabled={consolidating}
        className="btn-ghost"
        title="Run decay + dedup + merge"
      >
        <Sparkles size={14} className={consolidating ? "animate-pulse" : ""} />
        <span className="hidden sm:inline">Consolidate</span>
      </button>

      <button
        onClick={toggleTheme}
        className="btn-ghost"
        title={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
        aria-label="Toggle theme"
      >
        {theme === "dark" ? <Sun size={14} /> : <Moon size={14} />}
      </button>

      <button
        onClick={() => setQuickAddOpen(true)}
        className="btn-primary"
        title="Quick add (⌘K)"
      >
        <Plus size={14} />
        <span>Remember</span>
        <span className="kbd ml-1">⌘K</span>
      </button>
    </header>
  );
}
