import { useApp } from "../lib/store";
import { MemoryCard } from "../components/MemoryCard";
import { Heatmap } from "../components/Heatmap";
import { Sparkles, Activity, Database, FolderGit2, Bot, ArrowUpRight } from "lucide-react";
import { bytes, MEM_TYPE_META } from "../lib/format";
import { useMemo } from "react";

export function Overview() {
  const stats = useApp((s) => s.stats);
  const memories = useApp((s) => s.memories);
  const activity = useApp((s) => s.activity);
  const setView = useApp((s) => s.setView);
  const currentProjectId = useApp((s) => s.currentProjectId);
  const setSelected = useApp((s) => s.setSelectedMemoryUid);
  const selectedMemoryUid = useApp((s) => s.selectedMemoryUid);
  const projects = useApp((s) => s.projects);
  const agents = useApp((s) => s.agents);
  const recent = useMemo(
    () => memories.filter((m) => m.project_id === currentProjectId).slice(0, 6),
    [memories, currentProjectId]
  );

  // Build 12-week heatmap from activity
  const heatmap = useMemo(() => {
    const days = 12 * 7;
    const now = Date.now();
    const buckets = new Array(days).fill(0);
    for (const a of activity) {
      const idx = days - 1 - Math.floor((now - a.created_at) / (24 * 3600 * 1000));
      if (idx >= 0 && idx < days) buckets[idx]++;
    }
    return buckets;
  }, [activity]);

  // Type breakdown
  const typeData = stats?.by_type ?? [];
  const total = Math.max(1, typeData.reduce((a, [, n]) => a + n, 0));
  const typeColors: Record<string, string> = {
    fact: "bg-[var(--type-fact-dot)]",
    decision: "bg-[var(--type-decision-dot)]",
    preference: "bg-[var(--type-preference-dot)]",
    pattern: "bg-[var(--type-pattern-dot)]",
    episode: "bg-[var(--type-episode-dot)]",
    reflection: "bg-[var(--type-reflection-dot)]",
    code: "bg-[var(--type-code-dot)]",
  };

  return (
    <div className="mx-auto max-w-6xl space-y-8 p-8 animate-fade_in">
      {/* Hero */}
      <div>
        <div className="font-mono text-[11px] uppercase tracking-widest text-text-dim">
          {greeting()}, {dateString()}
        </div>
        <h2 className="mt-1 font-serif text-3xl font-medium text-balance text-text">
          Your memory layer at a glance.
        </h2>
        <p className="mt-1 max-w-2xl text-sm text-text-muted text-pretty">
          Local-first, turbovec-compressed memory for every AI agent you run.
          Search, browse, and project-isolate what your tools know.
        </p>
      </div>

      {/* First-run onboarding */}
      {projects.length === 0 && agents.length === 0 && (
        <div className="card border-accent/30 p-6">
          <h3 className="font-serif text-lg">Set up biTurbo in three steps</h3>
          <ol className="mt-3 space-y-2 text-sm text-text-muted">
            <li className="flex items-start gap-2">
              <span className="font-mono text-xs text-accent">1</span>
              <span>
                Create a project and point it at a repository —{" "}
                <button onClick={() => setView("projects")} className="text-accent underline underline-offset-2">
                  go to Projects
                </button>
              </span>
            </li>
            <li className="flex items-start gap-2">
              <span className="font-mono text-xs text-accent">2</span>
              <span>Run “Re-index code” so agents can search your codebase</span>
            </li>
            <li className="flex items-start gap-2">
              <span className="font-mono text-xs text-accent">3</span>
              <span>
                Connect your AI agent via MCP —{" "}
                <button onClick={() => setView("settings")} className="text-accent underline underline-offset-2">
                  Settings → one-click install
                </button>
              </span>
            </li>
          </ol>
        </div>
      )}

      {/* Stats row */}
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard
          icon={Database}
          label="Memories"
          value={(stats?.total_memories ?? 0).toLocaleString()}
          hint={`${stats?.recent_writes_7d ?? 0} this week`}
        />
        <StatCard
          icon={FolderGit2}
          label="Projects"
          value={(stats?.total_projects ?? 0).toLocaleString()}
          hint={`${(stats?.by_project ?? []).filter(([, n]) => n > 0).length} active`}
        />
        <StatCard
          icon={Bot}
          label="Agents"
          value={(stats?.total_agents ?? 0).toLocaleString()}
          hint={`${stats?.recent_reads_7d ?? 0} searches · 7d`}
        />
        <StatCard
          icon={Activity}
          label="Index size"
          value={bytes(stats?.index_bytes ?? 0)}
          hint="turbovec · bit_width=4"
        />
      </div>

      {/* Mid row: types + heatmap */}
      <div className="grid gap-3 md:grid-cols-2">
        {/* Types */}
        <div className="card p-5">
          <div className="mb-4 flex items-baseline justify-between">
            <h3 className="font-serif text-lg">Memory types</h3>
            <button
              onClick={() => setView("memories")}
              className="text-xs text-text-dim hover:text-accent"
            >
              browse →
            </button>
          </div>
          <div className="space-y-2.5">
            {typeData.length === 0 && (
              <div className="text-sm text-text-dim">No memories yet.</div>
            )}
            {typeData.map(([t, n]) => {
              const meta = MEM_TYPE_META[t];
              const pct = (n / total) * 100;
              return (
                <button
                  key={t}
                  onClick={() => setView("memories")}
                  className="block w-full text-left transition hover:opacity-80"
                >
                  <div className="mb-1 flex items-baseline justify-between text-xs">
                    <span className="flex items-center gap-2 capitalize text-text-muted">
                      <span
                        className={`h-1.5 w-1.5 rounded-full ${typeColors[t] ?? "bg-text-dim"}`}
                      />
                      {meta?.label ?? t}
                    </span>
                    <span className="font-mono text-text-dim">
                      {n} · {pct.toFixed(0)}%
                    </span>
                  </div>
                  <div className="h-1 overflow-hidden rounded-full bg-surface-2">
                    <div
                      className={`h-full ${typeColors[t] ?? "bg-text-dim"}`}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                </button>
              );
            })}
          </div>
        </div>

        {/* Heatmap */}
        <div className="card p-5">
          <div className="mb-4 flex items-baseline justify-between">
            <h3 className="font-serif text-lg">Activity · 12 weeks</h3>
            <span className="font-mono text-[10px] text-text-dim">
              {activity.length} recent
            </span>
          </div>
          <div className="overflow-x-auto">
            <Heatmap values={heatmap} weeks={12} />
          </div>
          <div className="mt-4 flex items-center gap-2 text-[10px] text-text-dim">
            <span>less</span>
            <div className="flex gap-0.5">
              <div className="h-2 w-2 rounded-sm bg-surface-2" />
              <div className="h-2 w-2 rounded-sm bg-accent/20" />
              <div className="h-2 w-2 rounded-sm bg-accent/40" />
              <div className="h-2 w-2 rounded-sm bg-accent/60" />
              <div className="h-2 w-2 rounded-sm bg-accent" />
            </div>
            <span>more</span>
          </div>
        </div>
      </div>

      {/* Recent memories */}
      <div>
        <div className="mb-3 flex items-baseline justify-between">
          <h3 className="font-serif text-lg">Recent memories</h3>
          <button
            onClick={() => setView("memories")}
            className="inline-flex items-center gap-1 text-xs text-text-dim hover:text-accent"
          >
            all <ArrowUpRight size={11} />
          </button>
        </div>
        {recent.length === 0 ? (
          <div className="card flex flex-col items-center justify-center p-12 text-center">
            <Sparkles className="mb-2 text-text-dim" size={20} />
            <div className="text-sm text-text-muted">No memories in this project yet.</div>
            <div className="mt-1 text-xs text-text-dim">
              Press <span className="kbd">⌘K</span> to remember something.
            </div>
          </div>
        ) : (
          <div className="grid gap-2 md:grid-cols-2">
            {recent.map((m) => (
              <MemoryCard
                key={m.uid}
                memory={m}
                active={selectedMemoryUid === m.uid}
                onClick={() => {
                  setSelected(m.uid);
                  setView("memories");
                }}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
}: {
  icon: import("lucide-react").LucideIcon;
} & {
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <div className="card p-4">
      <div className="flex items-center gap-2 text-text-dim">
        <Icon size={13} />
        <span className="text-[10px] uppercase tracking-widest">{label}</span>
      </div>
      <div className="mt-1.5 font-serif text-2xl font-medium text-text">{value}</div>
      {hint && <div className="mt-0.5 font-mono text-[10px] text-text-dim">{hint}</div>}
    </div>
  );
}

function greeting() {
  const h = new Date().getHours();
  if (h < 5) return "Late night";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

function dateString() {
  return new Date().toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
}
