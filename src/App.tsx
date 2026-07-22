import { useEffect, useState } from "react";
import { useApp } from "./lib/store";
import { Sidebar } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { QuickAdd } from "./components/QuickAdd";
import { Overview } from "./views/Overview";
import { Memories } from "./views/Memories";
import { Projects } from "./views/Projects";
import { Graph } from "./views/Graph";
import { Agents } from "./views/Agents";
import { Settings } from "./views/Settings";
import { Runtime } from "./views/Runtime";
import { Toast } from "./components/Toast";
import { ConfirmModalHost } from "./components/ConfirmModal";
import { ContextMenuHost } from "./components/ContextMenu";

export default function App() {
  const view = useApp((s) => s.view);
  const currentProjectId = useApp((s) => s.currentProjectId);
  const [ready, setReady] = useState(false);

  const bootstrapOnce = useApp((s) => s.bootstrapOnce);
  const refreshMemories = useApp((s) => s.refreshMemories);
  const refreshTags = useApp((s) => s.refreshTags);
  const refreshGraph = useApp((s) => s.refreshGraph);

  // Single batched IPC call on mount — replaces 7 sequential calls.
  useEffect(() => {
    bootstrapOnce()
      .catch((e) => console.error("bootstrap failed", e))
      .finally(() => setReady(true));
  }, [bootstrapOnce]);

  // Re-fetch project-scoped data when the active project changes.
  useEffect(() => {
    if (!ready) return;
    refreshMemories();
    refreshTags().catch(() => {});
    refreshGraph().catch(() => {});
  }, [currentProjectId, ready, refreshMemories, refreshTags, refreshGraph]);

  // Global keyboard
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const meta = e.metaKey || e.ctrlKey;
      if (meta && e.key === "k") {
        e.preventDefault();
        useApp.getState().setQuickAddOpen(true);
      } else if (meta && e.key === "/") {
        e.preventDefault();
        document.getElementById("global-search")?.focus();
      } else if (e.key === "Escape") {
        useApp.getState().setQuickAddOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!ready) {
    return (
      <div className="flex h-screen items-center justify-center text-text-muted">
        Loading biTurbo…
      </div>
    );
  }

  return (
    <div className="flex h-screen overflow-hidden bg-bg text-text">
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <TopBar />
        <main className="flex-1 overflow-y-auto">
          {view === "overview" && <Overview />}
          {view === "memories" && <Memories />}
          {view === "projects" && <Projects />}
          {view === "graph" && <Graph />}
          {view === "agents" && <Agents />}
          {view === "runtime" && <Runtime />}
          {view === "settings" && <Settings />}
        </main>
      </div>
      <QuickAdd />
      <Toast />
      <ConfirmModalHost />
      <ContextMenuHost />
    </div>
  );
}
