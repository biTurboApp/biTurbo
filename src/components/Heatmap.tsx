import clsx from "clsx";

export function Heatmap({
  values,
  weeks = 12,
}: {
  values: number[]; // length = weeks*7, oldest first
  weeks?: number;
}) {
  const total = weeks * 7;
  const padded = [...Array(Math.max(0, total - values.length)).fill(0), ...values];
  const max = Math.max(1, ...padded);
  const dayLabels = ["M", "", "W", "", "F", "", ""];

  return (
    <div className="flex gap-1">
      <div className="flex flex-col justify-between py-0.5 text-[9px] text-text-dim">
        {dayLabels.map((d, i) => (
          <div key={i} className="h-3 leading-3">
            {d}
          </div>
        ))}
      </div>
      <div
        className="grid gap-1"
        style={{ gridTemplateColumns: `repeat(${weeks}, minmax(0, 1fr))` }}
      >
        {Array.from({ length: weeks }).map((_, week) => (
          <div key={week} className="flex flex-col gap-1">
            {Array.from({ length: 7 }).map((_, day) => {
              const v = padded[week * 7 + day] || 0;
              const intensity = v / max;
              const daysAgo = total - 1 - (week * 7 + day);
              const cellDate = new Date(Date.now() - daysAgo * 86400e3);
              const label =
                daysAgo === 0
                  ? `Today · ${v} actions`
                  : `${cellDate.toLocaleDateString(undefined, {
                      weekday: "short",
                      month: "short",
                      day: "numeric",
                    })} · ${v} actions`;
              return (
                <div
                  key={day}
                  title={label}
                  className={clsx(
                    "h-3 w-3 rounded-sm transition",
                    v === 0
                      ? "bg-surface-2"
                      : intensity < 0.25
                        ? "bg-accent/20"
                        : intensity < 0.5
                          ? "bg-accent/40"
                          : intensity < 0.75
                            ? "bg-accent/60"
                            : "bg-accent"
                  )}
                />
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
