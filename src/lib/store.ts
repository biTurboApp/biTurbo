import { create } from "zustand";
import type {
  Memory,
  Project,
  Stats,
  AgentEntry,
  ActivityEntry,
  GraphData,
  IngestProgress,
} from "./types";
import { api } from "./api";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ConfirmOptions } from "../components/ConfirmModal";
import type { ContextMenuItem } from "../components/ContextMenu";

export type View = "overview" | "memories" | "projects" | "graph" | "agents" | "settings";
export type Theme = "dark" | "light";

interface AppStore {
  view: View;
  setView: (v: View) => void;

  theme: Theme;
  setTheme: (t: Theme) => void;
  toggleTheme: () => void;

  projects: Project[];
  currentProjectId: string;
  setCurrentProjectId: (id: string) => void;
  refreshProjects: () => Promise<void>;

  agents: AgentEntry[];
  refreshAgents: () => Promise<void>;

  stats: Stats | null;
  refreshStats: () => Promise<void>;

  activity: ActivityEntry[];
  refreshActivity: () => Promise<void>;

  searchQuery: string;
  setSearchQuery: (q: string) => void;

  tags: [string, number][];
  refreshTags: () => Promise<void>;

  memories: Memory[];
  selectedMemoryUid: string | null;
  setSelectedMemoryUid: (uid: string | null) => void;
  /** Full record for a selected uid that is not present in any loaded list. */
  hydratedSelected: Memory | null;
  selectMemoryByUid: (uid: string) => Promise<void>;
  memoryOffset: number;
  hasMoreMemories: boolean;
  loadMoreMemories: () => Promise<void>;
  refreshMemories: () => Promise<void>;

  quickAddOpen: boolean;
  setQuickAddOpen: (open: boolean) => void;

  toast: { kind: "ok" | "err" | "info"; text: string } | null;
  showToast: (t: { kind: "ok" | "err" | "info"; text: string }) => void;
  clearToast: () => void;

  graph: GraphData | null;
  refreshGraph: () => Promise<void>;

  ingestJobs: Record<string, IngestProgress>;
  startIngest: (project_id: string, root_path: string) => Promise<string>;
  cancelIngest: (job_id: string) => Promise<void>;

  bootstrapLoaded: boolean;
  bootstrapOnce: () => Promise<void>;

  confirmState: ConfirmOptions | null;
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  resolveConfirm: () => void;
  cancelConfirm: () => void;

  contextMenu: { x: number; y: number; items: ContextMenuItem[] } | null;
  showContextMenu: (x: number, y: number, items: ContextMenuItem[]) => void;
  closeContextMenu: () => void;
}

const THEME_KEY = "biturbo.theme";

function readStoredTheme(): Theme {
  if (typeof window === "undefined") return "dark";
  try {
    const v = window.localStorage.getItem(THEME_KEY);
    if (v === "light" || v === "dark") return v;
  } catch {
    /* ignore */
  }
  if (typeof window.matchMedia === "function") {
    if (window.matchMedia("(prefers-color-scheme: light)").matches) return "light";
  }
  return "dark";
}

function applyThemeToDom(t: Theme) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (t === "light") root.classList.add("light");
  else root.classList.remove("light");
  root.style.colorScheme = t;
}

export const useApp = create<AppStore>((set, get) => ({
  view: "overview",
  setView: (v) => set({ view: v }),

  theme: readStoredTheme(),
  setTheme: (t) => {
    try {
      window.localStorage.setItem(THEME_KEY, t);
    } catch {
      /* ignore */
    }
    applyThemeToDom(t);
    set({ theme: t });
  },
  toggleTheme: () => {
    const next: Theme = get().theme === "dark" ? "light" : "dark";
    get().setTheme(next);
  },

  projects: [],
  currentProjectId: "default",
  setCurrentProjectId: (id) => set({ currentProjectId: id, selectedMemoryUid: null }),
  refreshProjects: async () => {
    const projects = await api.listProjects();
    set({ projects });
    // If the active project no longer exists (e.g. just deleted), fall
    // back to a surviving project so scoped views don't keep filtering
    // by a dead id.
    const current = get().currentProjectId;
    if ((!current || !projects.some((p) => p.id === current)) && projects.length > 0) {
      const fallback = projects.find((p) => p.id === "default") ?? projects[0];
      set({ currentProjectId: fallback.id, selectedMemoryUid: null });
    }
  },

  agents: [],
  refreshAgents: async () => set({ agents: await api.listAgents() }),

  stats: null,
  refreshStats: async () => set({ stats: await api.stats() }),

  activity: [],
  refreshActivity: async () => set({ activity: await api.recentActivity(1000) }),
  searchQuery: "",
  setSearchQuery: (q) => set({ searchQuery: q }),

  tags: [],
  refreshTags: async () => {
    const tags = await api.listTags(get().currentProjectId);
    set({ tags });
  },

  memories: [],
  selectedMemoryUid: null,
  setSelectedMemoryUid: (uid) => set({ selectedMemoryUid: uid }),
  hydratedSelected: null,
  selectMemoryByUid: async (uid) => {
    set({ selectedMemoryUid: uid, hydratedSelected: null });
    try {
      const m = await api.getMemory(uid);
      if (get().selectedMemoryUid === uid) {
        set({ hydratedSelected: m });
      }
    } catch (e) {
      get().showToast({ kind: "err", text: String(e) });
    }
  },
  memoryOffset: 0,
  hasMoreMemories: false,
  loadMoreMemories: async () => {
    const offset = get().memoryOffset;
    const batch = await api.listMemories({
      project_id: get().currentProjectId,
      limit: 50,
      offset,
    });
    set((s) => {
      const combined = [...s.memories, ...batch];
      // Cap frontend memory usage: keep the newest 500 memories.
      const trimmed = combined.length > 500 ? combined.slice(-500) : combined;
      return {
        memories: trimmed,
        memoryOffset: offset + batch.length,
        hasMoreMemories: batch.length === 50,
      };
    });
  },
  refreshMemories: async () => {
    const mems = await api.listMemories({
      project_id: get().currentProjectId,
      limit: 50,
      offset: 0,
    });
    set({
      memories: mems,
      memoryOffset: mems.length,
      hasMoreMemories: mems.length === 50,
    });
  },

  quickAddOpen: false,
  setQuickAddOpen: (open) => set({ quickAddOpen: open }),

  toast: null,
  showToast: (t) => {
    set({ toast: t });
    setTimeout(() => {
      if (get().toast === t) set({ toast: null });
    }, 3500);
  },
  clearToast: () => set({ toast: null }),

  graph: null,
  refreshGraph: async () => {
    const g = await api.getProjectGraph(get().currentProjectId);
    set({ graph: g });
  },

  ingestJobs: {},
  startIngest: async (project_id, root_path) => {
    const job = await api.ingestProject(project_id, root_path);
    set((s) => ({
      ingestJobs: {
        ...s.ingestJobs,
        [job.job_id]: {
          project_id,
          phase: "queued",
          current: 0,
          total: 1,
          file: null,
          chunks_so_far: 0,
        },
      },
    }));
    return job.job_id;
  },
  cancelIngest: async (job_id) => {
    await api.cancelOperation(job_id);
    set((s) => {
      const { [job_id]: _, ...rest } = s.ingestJobs;
      return { ingestJobs: rest };
    });
  },

  bootstrapLoaded: false,
  bootstrapOnce: async () => {
    if (get().bootstrapLoaded) return;
    const b = await api.bootstrap();
    const projects = b.projects;
    const currentProjectId =
      projects.find((p) => p.indexed_count > 0)?.id ??
      projects[0]?.id ??
      "default";
    set({
      stats: b.stats,
      projects,
      currentProjectId,
      activity: b.recent,
      tags: b.tags,
      agents: b.agents,
      bootstrapLoaded: true,
    });
  },

  confirmState: null,
  confirm: (opts) => {
    set({ confirmState: opts });
    return new Promise<boolean>((resolve) => {
      registerConfirmResolver(resolve);
    });
  },
  resolveConfirm: () => {
    const r = takeConfirmResolver();
    r?.(true);
    set({ confirmState: null });
  },
  cancelConfirm: () => {
    const r = takeConfirmResolver();
    r?.(false);
    set({ confirmState: null });
  },

  contextMenu: null,
  showContextMenu: (x, y, items) => set({ contextMenu: { x, y, items } }),
  closeContextMenu: () => set({ contextMenu: null }),
}));

// The pending confirm resolver lives outside zustand state on purpose:
// if it lived in state, every confirm-related state change would
// re-render every subscriber, even those that only care about other
// state. With a module-local slot, only the modal itself re-renders.
let _pendingConfirmResolver: ((ok: boolean) => void) | null = null;
function registerConfirmResolver(r: (ok: boolean) => void) {
  _pendingConfirmResolver = r;
}
function takeConfirmResolver(): ((ok: boolean) => void) | null {
  const r = _pendingConfirmResolver;
  _pendingConfirmResolver = null;
  return r;
}

/**
 * Imperative confirm helper. Resolves to true on confirm, false on
 * cancel/Escape/backdrop click. Use from any component:
 *
 *   const ok = await useConfirm()({ title: "Delete?", body: "..." });
 */
export function useConfirm() {
  return useApp((s) => s.confirm);
}

export function useContextMenu() {
  return useApp((s) => s.showContextMenu);
}

if (typeof window !== "undefined") {
  applyThemeToDom(readStoredTheme());
  let unlistens: UnlistenFn[] = [];
  void (async () => {
    unlistens.push(
      await listen<IngestProgress>("ingest:progress", (e) => {
        const p = e.payload;
        useApp.setState((s) => ({
          ingestJobs: {
            ...s.ingestJobs,
            [p.project_id]: p,
          },
        }));
      }),
    );
    unlistens.push(
      await listen<{
        job_id: string;
        project_id: string;
        files_indexed: number;
        chunks_indexed: number;
        edges_created: number;
        elapsed_ms: number;
      }>("ingest:done", (e) => {
        const d = e.payload;
        useApp.setState((s) => ({
          ingestJobs: {
            ...s.ingestJobs,
            [d.project_id]: {
              ...s.ingestJobs[d.project_id],
              phase: "done",
              current: s.ingestJobs[d.project_id]?.total ?? 0,
            } as IngestProgress,
          },
        }));
        setTimeout(() => {
          useApp.setState((s) => {
            const { [d.project_id]: _, ...rest } = s.ingestJobs;
            return { ingestJobs: rest };
          });
        }, 1500);
        useApp.getState().showToast({
          kind: "ok",
          text: `Indexed ${d.files_indexed} files · ${d.chunks_indexed} chunks · ${Math.round(d.elapsed_ms / 100) / 10}s`,
        });
        void useApp.getState().refreshStats();
        void useApp.getState().refreshProjects();
        if (useApp.getState().currentProjectId === d.project_id) {
          void useApp.getState().refreshGraph();
        }
      }),
    );
    unlistens.push(
      await listen<{ job_id: string; project_id: string; error: string }>(
        "ingest:error",
        (e) => {
          const d = e.payload;
          setTimeout(() => {
            useApp.setState((s) => {
              const { [d.project_id]: _, ...rest } = s.ingestJobs;
              return { ingestJobs: rest };
            });
          }, 1500);
          useApp.getState().showToast({
            kind: "err",
            text: `Ingest failed: ${d.error}`,
          });
        },
      ),
    );
  })();
  window.addEventListener("beforeunload", () => unlistens.forEach((u) => u()));
}
