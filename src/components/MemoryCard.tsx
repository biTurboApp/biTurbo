import type { Memory, RecallExplanation } from "../lib/types";
import { MEM_TYPE_META, timeAgo, importanceDots, truncatePath, stripLeadingPathComment } from "../lib/format";
import { FileCode2, Hash, ThumbsDown, ThumbsUp } from "lucide-react";
import clsx from "clsx";
import { memo } from "react";
import type { ContextMenuItem } from "./ContextMenu";
import { useContextMenu } from "../lib/store";
import { CodeBlock } from "./CodeBlock";

interface MemoryCardProps {
  memory: Memory;
  active?: boolean;
  onClick?: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  contextMenuItems?: ContextMenuItem[];
  explanation?: RecallExplanation;
  onFeedback?: (value: -1 | 1) => void;
}

export const MemoryCard = memo(function MemoryCard({
  memory,
  active,
  onClick,
  onContextMenu,
  contextMenuItems,
  explanation,
  onFeedback,
}: MemoryCardProps) {
  const meta = MEM_TYPE_META[memory.mem_type] ?? MEM_TYPE_META.fact;
  const dots = importanceDots(memory.importance);
  const isCode = memory.mem_type === "code";

  const showMenu = useContextMenu();
  const handleContext =
    onContextMenu ??
    (contextMenuItems
      ? (e: React.MouseEvent) => {
          e.preventDefault();
          e.stopPropagation();
          showMenu(e.clientX, e.clientY, contextMenuItems);
        }
      : undefined);

  // Keyboard parity with mouse: Enter/Space activate, Shift+F10 or the
  // Menu key opens the context menu at the card.
  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick?.();
      return;
    }
    if ((e.shiftKey && e.key === "F10") || e.key === "ContextMenu") {
      e.preventDefault();
      if (!contextMenuItems) return;
      const rect = e.currentTarget.getBoundingClientRect();
      showMenu(rect.left + rect.width / 2, rect.top + rect.height / 2, contextMenuItems);
    }
  }

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={handleKeyDown}
      onContextMenu={handleContext}
      className={clsx("memory-card", active && "active")}>

      <div className="mb-2 flex items-center gap-2">
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
        {memory.superseded_by && (
          <span className="rounded-full border border-border-subtle px-1.5 py-0.5 text-[10px] text-text-dim">
            superseded
          </span>
        )}
        <span className="ml-auto font-mono text-[10px] text-text-dim">
          {timeAgo(memory.created_at)}
        </span>
      </div>

      {isCode ? (
        <CodeBlock
          code={stripLeadingPathComment(memory.content, memory.file_path)}
          maxLines={4}
        />
      ) : (
        <div className="line-clamp-3 text-sm leading-relaxed text-text text-pretty">
          {memory.content}
        </div>
      )}

      {isCode && memory.file_path && (
        <div className="code-chip mt-2 text-[11px]" title={memory.file_path}>
          <FileCode2 size={11} className="shrink-0" />
          <span className="code-chip-path">{truncatePath(memory.file_path)}</span>
          {memory.start_line && (
            <span className="code-chip-range">
              L{memory.start_line}
              {memory.end_line && memory.end_line !== memory.start_line
                ? `\u2013${memory.end_line}`
                : ""}
            </span>
          )}
        </div>
      )}

      <div className="mt-3 flex items-center gap-2 text-[11px] text-text-muted">
        {memory.tags.slice(0, 3).map((t) => (
          <span key={t} className="inline-flex items-center gap-1 text-text-dim">
            <Hash size={9} />
            {t}
          </span>
        ))}

        <div className="ml-auto flex items-center gap-2">
          {explanation && (
            <span
              className="font-mono text-[9px] text-text-dim"
              title={`Matched: ${explanation.matched_terms.join(", ") || "semantic only"}`}
            >
              {explanation.vector_rank ? `v#${explanation.vector_rank}` : ""}
              {explanation.vector_rank && explanation.fts_rank ? " · " : ""}
              {explanation.fts_rank ? `text#${explanation.fts_rank}` : ""}
            </span>
          )}
          {onFeedback && (
            <span className="flex items-center gap-0.5">
              <button
                type="button"
                title="Useful result"
                className="rounded p-1 text-text-dim hover:bg-accent/10 hover:text-accent"
                onClick={(event) => {
                  event.stopPropagation();
                  onFeedback(1);
                }}
              >
                <ThumbsUp size={10} />
              </button>
              <button
                type="button"
                title="Not useful"
                className="rounded p-1 text-text-dim hover:bg-danger/10 hover:text-danger"
                onClick={(event) => {
                  event.stopPropagation();
                  onFeedback(-1);
                }}
              >
                <ThumbsDown size={10} />
              </button>
            </span>
          )}
          {memory.source_agent && (
            <span className="font-mono text-[10px] text-text-dim">
              {memory.source_agent}
            </span>
          )}
          <div className="flex items-center gap-0.5" title={`importance ${memory.importance.toFixed(2)}`}>
            {Array.from({ length: 5 }).map((_, i) => (
              <span
                key={i}
                className={clsx(
                  "h-1 w-1 rounded-full",
                  i < dots ? "bg-accent" : "bg-text-dim/40"
                )}
              />
            ))}
          </div>
        </div>
      </div>
    </div>
  );
});
