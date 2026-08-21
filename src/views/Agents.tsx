import { useState } from "react";
import { useApp } from "../lib/store";
import { api } from "../lib/api";
import { Bot, Plus, RefreshCw } from "lucide-react";
import { timeAgo, friendlyError } from "../lib/format";

const KINDS = ["mavis", "claude-code", "cursor", "cline", "custom"];

export function Agents() {
  const agents = useApp((s) => s.agents);
  const refreshAgents = useApp((s) => s.refreshAgents);
  const showToast = useApp((s) => s.showToast);

  const [name, setName] = useState("");
  const [kind, setKind] = useState(KINDS[0]);
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  async function refresh() {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await refreshAgents();
    } catch (e) {
      showToast({ kind: "err", text: String(e) });
    } finally {
      setRefreshing(false);
    }
  }

  async function register() {
    if (!name.trim()) return;
    setBusy(true);
    try {
      await api.registerAgent(name.trim(), kind);
      setName("");
      await refreshAgents();
      showToast({ kind: "ok", text: `Registered ${name}` });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mx-auto max-w-4xl space-y-6 p-8 animate-fade_in">
      <div className="flex items-baseline justify-between">
        <div>
          <h2 className="font-serif text-2xl">Agents</h2>
          <p className="mt-1 text-sm text-text-muted">
            AI agents (Mavis, Cursor, Claude Code, Cline…) that have called biTurbo via MCP or
            directly. Each agent's reads and writes are attributed automatically.
          </p>
        </div>
conflict://4
        <button
          onClick={() => void refresh()}
          disabled={refreshing}
          className="btn-ghost"
          aria-label="Refresh agents"
          title="Refresh agents"
        >
          <RefreshCw size={13} className={refreshing ? "animate-spin" : ""} />
        </button>
      </div>

      {/* Register form */}
      <form
        className="card p-4"
        onSubmit={(e) => {
          e.preventDefault();
          void register();
        }}
      >
        <div className="mb-2 text-[10px] uppercase tracking-widest text-text-dim">
          Register a new agent
        </div>
        <div className="flex gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Mavis"
            className="input flex-1"
          />
          <input
            value={kind}
            onChange={(e) => setKind(e.target.value)}
            placeholder="kind"
            list="agent-kinds"
            className="input w-44"
          />
          <datalist id="agent-kinds">
            {KINDS.map((k) => (
              <option key={k} value={k} />
            ))}
          </datalist>
          <button type="submit" onClick={register} disabled={!name.trim() || busy} className="btn-primary">
            <Plus size={14} /> Register
          </button>
        </div>
        <div className="mt-2 text-[10px] text-text-dim">
          Agents auto-register on first MCP call; you can also register by hand here.
        </div>
      </form>

      {/* Agent list */}
      <div className="space-y-2">
        {agents.length === 0 && (
          <div className="card flex flex-col items-center justify-center p-12 text-center text-text-dim">
            <Bot size={28} className="mb-2 opacity-50" />
            <div className="text-sm">No agents registered yet.</div>
            <div className="mt-1 text-xs">
              Connect an agent via the MCP server (see Settings → MCP).
            </div>
          </div>
        )}
        {agents.map((a) => {
          // Honest activity tiers: only genuinely recent activity pulses.
          const age = Date.now() - a.last_seen;
          const isActiveNow = age < 5 * 60 * 1000;
          const isToday = age < 24 * 60 * 60 * 1000;
          const daysAgo = Math.floor(age / (24 * 60 * 60 * 1000));
          return (
            <div key={a.id} className="card flex items-center gap-3 p-4">
              <div
                className="flex h-9 w-9 items-center justify-center rounded-md bg-surface-2 text-accent"
              >
                <Bot size={16} />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex items-baseline gap-2">
                  <h3 className="font-serif text-base text-text">{a.name}</h3>
                  <span className="rounded-full border border-border bg-surface-2 px-1.5 py-0.5 text-[10px] font-mono text-text-muted">
                    {a.kind}
                  </span>
                </div>
                <div className="mt-0.5 text-[11px] text-text-dim">
                  last seen {timeAgo(a.last_seen)} · id <span className="font-mono">{a.id}</span>
                </div>
              </div>
              {isActiveNow && (
                <div className="flex items-center gap-1.5" title={`last seen ${timeAgo(a.last_seen)}`}>
                  <span className="relative flex h-2 w-2">
                    <span className="absolute inline-flex h-full w-full animate-pulse_dot rounded-full bg-success opacity-75" />
                    <span className="relative inline-flex h-2 w-2 rounded-full bg-success" />
                  </span>
                  <span className="text-[10px] uppercase tracking-widest text-text-dim">
                    active now
                  </span>
                </div>
              )}
              {!isActiveNow && isToday && (
                <span
                  className="flex items-center gap-1.5 text-[10px] uppercase tracking-widest text-text-dim"
                  title={`last seen ${timeAgo(a.last_seen)}`}
                >
                  <span className="inline-flex h-2 w-2 rounded-full bg-success/50" />
                  today
                </span>
              )}
              {!isActiveNow && !isToday && (
                <span
                  className="text-[10px] uppercase tracking-widest text-text-dim"
                  title={`last seen ${timeAgo(a.last_seen)}`}
                >
                  {daysAgo === 1 ? "yesterday" : `${daysAgo}d ago`}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
