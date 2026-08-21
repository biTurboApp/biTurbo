import { useEffect, useRef, useState } from "react";
import { useApp } from "../lib/store";
import { api } from "../lib/api";
import { X, Plus } from "lucide-react";
import clsx from "clsx";
import { friendlyError } from "../lib/format";

const TYPES = ["fact", "decision", "preference", "pattern", "episode", "reflection"] as const;

export function QuickAdd() {
  const open = useApp((s) => s.quickAddOpen);
  const setOpen = useApp((s) => s.setQuickAddOpen);
  const currentProjectId = useApp((s) => s.currentProjectId);
  const refreshMemories = useApp((s) => s.refreshMemories);
  const refreshStats = useApp((s) => s.refreshStats);
  const showToast = useApp((s) => s.showToast);

  const [content, setContent] = useState("");
  const [type, setType] = useState<(typeof TYPES)[number]>("fact");
  const [tags, setTags] = useState("");
  const [importance, setImportance] = useState(0.5);
  const [busy, setBusy] = useState(false);

  const panelRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const previouslyFocused = useRef<HTMLElement | null>(null);

  // Dialog focus management: move focus into the modal on open, restore
  // it to the opener on close.
  useEffect(() => {
    if (!open) return;
    previouslyFocused.current = (document.activeElement as HTMLElement) ?? null;
    const t = window.setTimeout(() => textareaRef.current?.focus(), 0);
    return () => {
      window.clearTimeout(t);
      const opener = previouslyFocused.current;
      if (opener && document.body.contains(opener)) {
        opener.focus();
      }
    };
  }, [open]);

  // Keep Tab cycling inside the dialog.
  function trapTab(e: React.KeyboardEvent) {
    if (e.key !== "Tab" || !panelRef.current) return;
    const focusables = panelRef.current.querySelectorAll<HTMLElement>(
      'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])',
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement as HTMLElement | null;
    const inside = active != null && panelRef.current.contains(active);
    if (e.shiftKey) {
      if (active === first || !inside) {
        e.preventDefault();
        last.focus();
      }
    } else {
      if (active === last || !inside) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  if (!open) return null;

  async function submit() {
    if (!content.trim() || busy) return;
    setBusy(true);
    try {
      await api.remember({
        content: content.trim(),
        mem_type: type,
        project_id: currentProjectId,
        tags: tags
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        importance,
        source_agent: "human",
      });
      setContent("");
      setTags("");
      setImportance(0.5);
      setType("fact");
      setOpen(false);
      await refreshMemories();
      await refreshStats();
      showToast({ kind: "ok", text: "Remembered" });
    } catch (e) {
      showToast({ kind: "err", text: friendlyError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 px-4 pt-[10vh] backdrop-blur-sm animate-fade_in"
      onClick={() => setOpen(false)}
      onKeyDown={trapTab}
    >
      <div
        ref={panelRef}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-labelledby="quickadd-title"
        className="w-full max-w-2xl overflow-hidden rounded-xl border border-border bg-surface shadow-2xl"
      >
        <div className="flex items-center justify-between border-b border-border-subtle px-4 py-3">
          <div id="quickadd-title" className="flex items-center gap-2 font-serif text-lg">
            <span>Remember</span>
            <span className="font-mono text-[10px] text-text-dim">⌘K</span>
          </div>
          <button onClick={() => setOpen(false)} className="btn-ghost p-1.5">
            <X size={14} />
          </button>
        </div>

        <div className="space-y-3 p-4">
          <textarea
            ref={textareaRef}
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="What should the agents remember?"
            rows={4}
            className="input resize-none text-sm"
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === "Enter") submit();
            }}
          />

          <div className="flex flex-wrap items-center gap-2">
            <div className="flex flex-wrap gap-1">
              {TYPES.map((t) => (
                <button
                  key={t}
                  onClick={() => setType(t)}
                  className={clsx(
                    "rounded-full px-2.5 py-0.5 text-xs transition",
                    type === t
                      ? "bg-accent text-bg"
                      : "border border-border bg-surface-2 text-text-muted hover:text-text"
                  )}
                >
                  {t}
                </button>
              ))}
            </div>

            <div className="ml-auto flex items-center gap-3">
              <input
                value={tags}
                onChange={(e) => setTags(e.target.value)}
                placeholder="tags, comma, separated"
                className="input w-48 py-1 text-xs"
              />
              <div className="flex items-center gap-2">
                <span className="font-mono text-[10px] text-text-dim">imp</span>
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.05"
                  value={importance}
                  onChange={(e) => setImportance(parseFloat(e.target.value))}
                  className="w-20 accent-accent"
                />
                <span className="w-7 text-right font-mono text-[10px] text-text-muted">
                  {importance.toFixed(2)}
                </span>
              </div>
            </div>
          </div>
        </div>

        <div className="flex items-center justify-between border-t border-border-subtle px-4 py-3">
          <div className="font-mono text-[10px] text-text-dim">
            {content.length} chars · project: {currentProjectId}
          </div>
          <div className="flex items-center gap-2">
            <span className="kbd">⌘⏎</span>
            <button
              onClick={submit}
              disabled={!content.trim() || busy}
              className="btn-primary"
            >
              <Plus size={14} /> Remember
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
