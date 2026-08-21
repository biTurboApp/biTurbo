import { useApp } from "../lib/store";
import clsx from "clsx";

/**
 * Toast stack. Queued (up to 4 visible), announced to assistive tech via
 * aria-live, with optional action buttons (e.g. Retry / Reveal).
 */
export function Toast() {
  const toasts = useApp((s) => s.toasts);
  const dismissToast = useApp((s) => s.dismissToast);
  if (toasts.length === 0) return null;
  return (
    <div
      className="fixed bottom-4 left-1/2 z-50 flex w-full max-w-md -translate-x-1/2 flex-col gap-2 px-4"
      role="status"
      aria-live="polite"
    >
      {toasts.map((t) => (
        <div
          key={t.id}
          onClick={() => dismissToast(t.id)}
          className={clsx(
            "flex cursor-pointer items-center gap-3 rounded-md border px-3 py-2 text-sm shadow-lg animate-fade_in",
            t.kind === "ok" && "border-success/30 bg-success/10 text-success",
            t.kind === "err" && "border-danger/30 bg-danger/10 text-danger",
            t.kind === "info" && "border-border bg-surface text-text",
          )}
        >
          <span className="min-w-0 flex-1 break-words">{t.text}</span>
          {t.action && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                t.action?.onClick();
                dismissToast(t.id);
              }}
              className={clsx(
                "shrink-0 rounded border border-current/40 px-2 py-0.5 text-xs font-medium transition hover:bg-current/10",
              )}
            >
              {t.action.label}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
