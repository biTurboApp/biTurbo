import { tokenizeCode } from "../lib/format";

const TOKEN_CLASS: Record<string, string> = {
  keyword: "italic",
  string: "italic",
  comment: "text-text-dim italic",
  number: "italic",
  plain: "text-text-muted",
};

function TokenSpan({ kind, children }: { kind: string; children: React.ReactNode }) {
  const style: React.CSSProperties = {};
  if (kind === "keyword" || kind === "string" || kind === "number") {
    style.color = `var(--token-${kind})`;
  }
  return (
    <span style={style} className={TOKEN_CLASS[kind] ?? ""}>
      {children}
    </span>
  );
}

interface CodeBlockProps {
  code: string;
  maxLines?: number;
  className?: string;
}

export function CodeBlock({ code, maxLines, className }: CodeBlockProps) {
  const lines = code.split("\n");
  const shown = maxLines ? lines.slice(0, maxLines) : lines;
  const truncated = maxLines != null && lines.length > maxLines;
  const lineClass = maxLines ? "code-block-line" : "code-block-line wrap";

  return (
    <pre className={`code-block ${className ?? ""}`}>
      <code>
        {shown.map((line, i) => (
          <div key={i} className={lineClass}>
            {tokenizeCode(line).map((tok, j) => (
              <TokenSpan key={j} kind={tok.kind}>
                {tok.text}
              </TokenSpan>
            ))}
            {line.length === 0 && "\u00A0"}
          </div>
        ))}
        {truncated && <div className="code-block-line text-text-dim">…</div>}
      </code>
    </pre>
  );
}
