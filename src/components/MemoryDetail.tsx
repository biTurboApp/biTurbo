import { useEffect, useState } from "react";
import type { Memory } from "../lib/types";
import { MEM_TYPE_META, timeAgo, shortDate, importanceDots, truncatePath, stripLeadingPathComment } from "../lib/format";
import { api } from "../lib/api";
import { useApp, useConfirm } from "../lib/store";
import { X, Trash2, Edit3, Save, FileCode2, Hash, ChevronDown, ChevronUp } from "lucide-react";
import clsx from "clsx";
import { CodeBlock } from "./CodeBlock";

export function MemoryDetail({ memory, onClose }: { memory: Memory; onClose: () => void }) {
  const [editing, setEditing] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [draft, setDraft] = useState(memory.content);
  const [draftTags, setDraftTags] = useState(memory.tags.join(", "));
  const [draftImp, setDraftImp] = useState(memory.importance);
  const [related, setRelated] = useState<{ uid: string; content: string; score: number }[]>([]);
  const refreshMemories = useApp((s) => s.refreshMemories);
  const refreshStats = useApp((s) => s.refreshStats);
  const showToast = useApp((s) => s.showToast);
  const setSelected = useApp((s) => s.setSelectedMemoryUid);
  const confirm = useConfirm();

  useEffect(() => {
    setDraft(memory.content);
    setDraftTags(memory.tags.join(", "));
    setDraftImp(memory.importance);
    setEditing(false);
    setExpanded(false);
  }, [memory.uid]);

  useEffect(() => {
    (async () => {
      try {
        const hits = await api.search({
          project_id: memory.project_id,
          query: memory.content.slice(0, 200),
          k: 6,
        });
        setRelated(
          hits
            .filter((h) => h.uid !== memory.uid)
            .slice(0, 5)
            .map((h) => ({ uid: h.uid, content: h.content, score: h.score }))
        );
      } catch {
        /* ignore */
      }
    })();
    // Only re-search when selecting a different memory, not on every edit.
  }, [memory.uid, memory.project_id]);

  async function save() {
    try {
      await api.update(memory.uid, {
        content: draft,
        tags: draftTags
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        importance: draftImp,
      });
      await refreshMemories();
      showToast({ kind: "ok", text: "Saved" });
      setEditing(false);
    } catch (e) {
      showToast({ kind: "err", text: String(e) });
    }
  }

  async function forget() {
    const ok = await confirm({
      title: "Forget this memory?",
      body: "It will be removed from the vector index too. This cannot be undone.",
      confirmLabel: "Forget",
    });
    if (!ok) return;
    try {
      await api.forget(memory.uid);
      setSelected(null);
      await Promise.all([refreshMemories(), refreshStats()]);
      showToast({ kind: "ok", text: "Forgotten" });
    } catch (e) {
      showToast({ kind: "err", text: String(e) });
    }
  }

  const meta = MEM_TYPE_META[memory.mem_type] ?? MEM_TYPE_META.fact;
  const dots = importanceDots(memory.importance);
  const isCode = memory.mem_type === "code";
  const bodyContent = isCode
    ? stripLeadingPathComment(memory.content, memory.file_path)
    : memory.content;
  const CODE_COLLAPSE_LINES = 14;
  const TEXT_COLLAPSE_CHARS = 220;
  const TEXT_COLLAPSE_LINES = 8;
  const isCollapsible = isCode
    ? bodyContent.split("\n").length > CODE_COLLAPSE_LINES
    : bodyContent.length > TEXT_COLLAPSE_CHARS || bodyContent.split("\n").length > TEXT_COLLAPSE_LINES;
  const collapsed = isCollapsible && !expanded;

  return (
    <div className="flex h-full flex-col">
      {/* Header */}
      <div className="flex items-start gap-2 border-b border-border-subtle p-4">
        <div className="flex-1">
          <div className="mb-1 flex items-center gap-2">
            <span
              className={clsx(
                "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider",
                meta.bg,
                meta.color
              )}
            >
              <span className={clsx("h-1 w-1 rounded-full", meta.dot)} />
              {meta.label}
            </span>
            <span className="font-mono text-[10px] text-text-dim">
              {memory.uid.slice(0, 8)}
            </span>
          </div>
        </div>
        <button onClick={onClose} className="btn-ghost p-1.5" aria-label="Close details" title="Close">
          <X size={14} />
        </button>
      </div>

      {/* Body */}
      <div className="flex-1 overflow-y-auto p-4">
        {editing ? (
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            rows={8}
            className="input resize-none font-sans text-sm"
            autoFocus
          />
        ) : isCode ? (
          <div className="relative">
            <CodeBlock code={bodyContent} maxLines={collapsed ? CODE_COLLAPSE_LINES : undefined} />
            {collapsed && (
              <div className="pointer-events-none absolute inset-x-0 bottom-0 h-10 rounded-b-md bg-gradient-to-t from-surface to-transparent" />
            )}
          </div>
        ) : (
          <div className="relative">
            <div
              className={clsx(
                "whitespace-pre-wrap text-sm leading-relaxed text-text text-pretty overflow-hidden",
                collapsed && "max-h-[160px]"
              )}
            >
              {memory.content}
            </div>
            {collapsed && (
              <div className="pointer-events-none absolute inset-x-0 bottom-0 h-10 bg-gradient-to-t from-surface to-transparent" />
            )}
          </div>
        )}

        {!editing && isCollapsible && (
          <button
            onClick={() => setExpanded((e) => !e)}
            className="mt-2 flex items-center gap-1 text-[11px] text-text-dim transition hover:text-text-muted"
          >
            {expanded ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
            {expanded ? "Show less" : "Show more"}
          </button>
        )}

        {/* Code location */}
        {memory.mem_type === "code" && memory.file_path && (
          <div className="code-chip mt-3 py-1.5 text-[12px]" title={memory.file_path}>
            <FileCode2 size={12} className="shrink-0" />
            <span className="code-chip-path">{truncatePath(memory.file_path, 56)}</span>
            {memory.start_line && (
              <span className="code-chip-range">
                L{memory.start_line}
                {memory.end_line && memory.end_line !== memory.start_line
                  ? `\u2013${memory.end_line}`
                  : ""}
              </span>
            )}
            {memory.language && (
              <span className="code-chip-lang">{memory.language}</span>
            )}
          </div>
        )}

        {/* Tags */}
        <div className="mt-4">
          <div className="mb-1.5 text-[10px] uppercase tracking-widest text-text-dim">
            Tags
          </div>
          {editing ? (
            <input
              value={draftTags}
              onChange={(e) => setDraftTags(e.target.value)}
              className="input"
              placeholder="comma-separated"
            />
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {memory.tags.length === 0 && (
                <span className="text-xs text-text-dim">—</span>
              )}
              {memory.tags.map((t) => (
                <span key={t} className="chip">
                  <Hash size={9} />
                  {t}
                </span>
              ))}
            </div>
          )}
        </div>

        {/* Importance slider */}
        <div className="mt-4">
          <div className="mb-1.5 flex items-center justify-between text-[10px] uppercase tracking-widest text-text-dim">
            <span>Importance</span>
            <span className="font-mono text-text-muted">
              {editing ? draftImp.toFixed(2) : memory.importance.toFixed(2)}
            </span>
          </div>
          {editing ? (
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              value={draftImp}
              onChange={(e) => setDraftImp(parseFloat(e.target.value))}
              className="w-full accent-accent"
            />
          ) : (
            <div className="flex items-center gap-0.5">
              {Array.from({ length: 5 }).map((_, i) => (
                <span
                  key={i}
                  className={clsx(
                    "h-1.5 w-6 rounded-full",
                    i < dots ? "bg-accent" : "bg-surface-2"
                  )}
                />
              ))}
            </div>
          )}
        </div>

        {/* Metadata grid */}
        <div className="mt-5 grid grid-cols-2 gap-3 border-t border-border-subtle pt-4 text-xs">
          <Meta label="Project" value={memory.project_id} mono />
          <Meta label="Source" value={memory.source_agent ?? "—"} mono />
          <Meta label="Created" value={shortDate(memory.created_at)} mono />
          <Meta label="Updated" value={shortDate(memory.updated_at)} mono />
          <Meta label="Accesses" value={String(memory.access_count)} mono />
          <Meta
            label="Last access"
            value={memory.last_access ? timeAgo(memory.last_access) : "—"}
          />
        </div>

        {/* Related */}
        {related.length > 0 && (
          <div className="mt-5 border-t border-border-subtle pt-4">
            <div className="mb-2 text-[10px] uppercase tracking-widest text-text-dim">
              Related
            </div>
            <div className="space-y-1.5">
              {related.map((r) => (
                <button
                  key={r.uid}
                  onClick={() => setSelected(r.uid)}
                  className="block w-full rounded-md border border-border-subtle bg-surface p-2 text-left text-[11px] text-text-muted transition hover:border-border hover:bg-surface-2"
                >
                  <div className="line-clamp-2 text-pretty">{r.content}</div>
                  <div className="mt-1 flex items-center gap-1.5">
                    <div className="h-1 w-12 overflow-hidden rounded-full bg-surface-2">
                      <div
                        className="h-full bg-accent"
                        style={{ width: `${Math.round(Math.min(1, Math.max(0, r.score)) * 100)}%` }}
                      />
                    </div>
                    <span className="font-mono text-[10px] text-text-dim">
                      {Math.round(r.score * 100)}% match
                    </span>
                  </div>
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* Footer actions */}
      <div className="flex items-center gap-2 border-t border-border-subtle p-3">
        {editing ? (
          <>
            <button onClick={save} className="btn-primary flex-1">
              <Save size={14} /> Save
            </button>
            <button onClick={() => setEditing(false)} className="btn-outline">
              Cancel
            </button>
          </>
        ) : (
          <>
            <button onClick={() => setEditing(true)} className="btn-outline flex-1">
              <Edit3 size={14} /> Edit
            </button>
            <button onClick={forget} className="btn-outline text-danger hover:bg-danger/10">
              <Trash2 size={14} /> Forget
            </button>
          </>
        )}
      </div>
    </div>
  );
}

function Meta({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-widest text-text-dim">
        {label}
      </div>
      <div className={clsx("mt-0.5 text-text-muted", mono && "font-mono")}>
        {value}
      </div>
    </div>
  );
}
