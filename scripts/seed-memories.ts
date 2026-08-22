#!/usr/bin/env -S node --experimental-strip-types --no-warnings
/**
 * Seed a handful of normal (non-code) memories into the active biTurbo store
 * so the text-card UI can be inspected without real agent traffic.
 *
 *   node --experimental-strip-types --no-warnings scripts/seed-memories.ts
 *
 * Optional: --bin=/path/to/biturbo-mcp (default: auto-discover)
 *           --project=<project_id>      (default: default project)
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

type RpcId = number;
interface RpcRequest {
  jsonrpc: "2.0";
  id: RpcId;
  method: string;
  params?: unknown;
}
interface RpcResponse {
  jsonrpc: "2.0";
  id: RpcId;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

const argv = process.argv.slice(2);
const arg = (k: string, def?: string) => {
  const a = argv.find((x) => x.startsWith(`--${k}=`));
  return a ? a.split("=", 2)[1] : def;
};

function findBinary(): string {
  const explicit = arg("bin");
  if (explicit) return explicit;
  const candidates = [
    resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp"),
    resolve(process.cwd(), "src-tauri/target/release/biturbo-mcp"),
  ];
  for (const c of candidates) if (existsSync(c)) return c;
  return "biturbo-mcp";
}

class McpClient {
  private proc: ChildProcessWithoutNullStreams;
  private buf = "";
  private pending = new Map<RpcId, (r: RpcResponse) => void>();
  private nextId = 1;

  constructor(bin: string) {
    this.proc = spawn(bin, [], { stdio: ["pipe", "pipe", "pipe"] });
    this.proc.stdout.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.proc.stderr.on("data", (chunk: Buffer) => {
      process.stderr.write(`[mcp-stderr] ${chunk.toString("utf8")}`);
    });
  }

  private onStdout(chunk: Buffer) {
    this.buf += chunk.toString("utf8");
    let idx: number;
    while ((idx = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, idx).trim();
      this.buf = this.buf.slice(idx + 1);
      if (!line) continue;
      try {
        const parsed = JSON.parse(line) as RpcResponse;
        if (parsed.id !== undefined && this.pending.has(parsed.id)) {
          this.pending.get(parsed.id)!(parsed);
          this.pending.delete(parsed.id);
        }
      } catch {
        /* ignore non-JSON lines */
      }
    }
  }

  private send(req: RpcRequest): Promise<RpcResponse> {
    return new Promise((resolveP, rejectP) => {
      const id = req.id;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectP(new Error(`RPC ${req.method} timed out`));
      }, 30_000);
      this.pending.set(id, (r) => {
        clearTimeout(timer);
        resolveP(r);
      });
      this.proc.stdin.write(JSON.stringify(req) + "\n");
    });
  }

  async call(method: string, params?: unknown): Promise<RpcResponse> {
    return this.send({ jsonrpc: "2.0", id: this.nextId++, method, params });
  }

  async initialize() {
    const r = await this.call("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "biturbo-seeder", version: "0.1.0" },
    });
    if (r.error) throw new Error(`initialize failed: ${r.error.message}`);
    this.proc.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n",
    );
  }

  async callTool(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const r = await this.call("tools/call", { name, arguments: args });
    if (r.error) throw new Error(`${name} failed: ${r.error.message}`);
    return r.result;
  }

  close() {
    try {
      this.proc.kill();
    } catch {
      /* ignore */
    }
  }
}

const MEMORIES: Array<{
  content: string;
  mem_type: string;
  tags: string[];
  importance: number;
  source_agent: string;
}> = [
  {
    content:
      "We decided to use Tailwind CSS for the whole biTurbo UI to keep the visual system consistent with the Tauri app. Custom component classes live in index.css and semantic color tokens are exposed as CSS variables.",
    mem_type: "decision",
    tags: ["ui", "tailwind", "css"],
    importance: 0.78,
    source_agent: "seed",
  },
  {
    content:
      "The biTurbo architecture uses turbovec for quantized embeddings, SQLite for canonical storage, and a background flusher that persists the vector index to disk. This keeps startup RSS low while still allowing fast semantic search.",
    mem_type: "fact",
    tags: ["architecture", "turbovec", "sqlite"],
    importance: 0.85,
    source_agent: "seed",
  },
  {
    content:
      "I prefer memory cards to show the type badge first, then the content preview, then the path/chip at the bottom. The importance dots should stay subtle and aligned to the right so they don't compete with the content.",
    mem_type: "preference",
    tags: ["ui", "memory-card", "design"],
    importance: 0.65,
    source_agent: "seed",
  },
  {
    content:
      "Code memories often store a redundant header comment like `// C:\\path\\file.ts:1-133` before the actual source. We should strip that header before rendering the code block, because the path chip already shows the file and line range.",
    mem_type: "pattern",
    tags: ["code", "memory-card", "parsing"],
    importance: 0.72,
    source_agent: "seed",
  },
  {
    content:
      "Last week we tried several syntax-highlighting libraries but ended up writing a tiny dependency-free tokenizer to avoid shipping heavy bundles. It covers keywords, strings, comments, and numbers well enough for preview cards.",
    mem_type: "episode",
    tags: ["ui", "code-block", "highlighting"],
    importance: 0.55,
    source_agent: "seed",
  },
  {
    content:
      "The Overview grid should surface a mix of recent memory types so users immediately see value beyond empty stats. The two-column layout on desktop needs to handle long code paths gracefully without breaking the card layout.",
    mem_type: "reflection",
    tags: ["overview", "ui", "layout"],
    importance: 0.6,
    source_agent: "seed",
  },
];

async function main() {
  const bin = findBinary();
  const projectId = arg("project");

  if (!existsSync(bin) && !arg("bin")) {
    console.error("Could not locate biturbo-mcp. Build it first:");
    console.error("  pnpm mcp:build");
    process.exit(2);
  }

  const client = new McpClient(bin);
  try {
    await client.initialize();

    for (const m of MEMORIES) {
      const args: Record<string, unknown> = {
        content: m.content,
        mem_type: m.mem_type,
        tags: m.tags,
        importance: m.importance,
        source_agent: m.source_agent,
      };
      if (projectId) args.project_id = projectId;

      await client.callTool("remember", args);
      console.log(`remembered ${m.mem_type}: ${m.content.slice(0, 40)}…`);
    }

    console.log(`\nSeeded ${MEMORIES.length} memories.`);
    if (!projectId) {
      console.log("They were stored in the default project. Switch to that project in the UI if needed.");
    } else {
      console.log(`Stored in project: ${projectId}`);
    }
    console.log("Refresh the app to see the new text cards.");
  } finally {
    client.close();
  }
}

main().catch((e) => {
  console.error("Seed failed:", e);
  process.exit(1);
});
