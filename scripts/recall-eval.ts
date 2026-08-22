#!/usr/bin/env -S node --experimental-strip-types --no-warnings
/**
 * Recall quality eval for biTurbo.
 *
 * Seeds a temporary project with golden memories, runs search + recall_for_context,
 * and reports hit@k plus basic context checks.
 *
 *   pnpm recall:eval
 *   pnpm recall:eval -- --bin=/path/to/biturbo-mcp
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

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

interface RpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

interface GoldenMemory {
  alias: string;
  content: string;
  mem_type: string;
  tags?: string[];
  importance?: number;
  file_path?: string;
  start_line?: number;
  end_line?: number;
  language?: string;
  supersedes_alias?: string;
}

interface GoldenCase {
  name: string;
  query: string;
  expected_aliases: string[];
  k?: number;
  must_include?: string[];
  must_exclude_aliases?: string[];
  mem_type?: string;
  expect_empty?: boolean;
}

interface GoldenFile {
  version: number;
  project: { id_prefix: string; name: string };
  memories: GoldenMemory[];
  isolation_probe?: GoldenMemory;
  cases: GoldenCase[];
}

const COL = {
  reset: "\x1b[0m",
  green: "\x1b[32m",
  red: "\x1b[31m",
  dim: "\x1b[2m",
  bold: "\x1b[1m",
};

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
    resolve(process.cwd(), "../src-tauri/target/debug/biturbo-mcp"),
    resolve(process.cwd(), "../src-tauri/target/release/biturbo-mcp"),
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
      if (process.env.MCP_TEST_VERBOSE) process.stderr.write(`[mcp-stderr] ${chunk.toString("utf8")}`);
    });
  }

  private onStdout(chunk: Buffer) {
    this.buf += chunk.toString("utf8");
    let idx: number;
    while ((idx = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, idx).trim();
      this.buf = this.buf.slice(idx + 1);
      if (!line) continue;
      let parsed: RpcResponse | RpcNotification;
      try {
        parsed = JSON.parse(line);
      } catch {
        if (process.env.MCP_TEST_VERBOSE) process.stderr.write(`[mcp-stdout-nonjson] ${line}\n`);
        continue;
      }
      if ("id" in parsed && parsed.id !== undefined && this.pending.has(parsed.id as RpcId)) {
        this.pending.get(parsed.id as RpcId)!(parsed as RpcResponse);
        this.pending.delete(parsed.id as RpcId);
      }
    }
  }

  private send(req: RpcRequest): Promise<RpcResponse> {
    return new Promise((resolveP, rejectP) => {
      const id = req.id;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectP(new Error(`RPC ${req.method} timed out: ${JSON.stringify(req.params)}`));
      }, 120_000);
      this.pending.set(id, (r) => {
        clearTimeout(timer);
        resolveP(r);
      });
      this.proc.stdin.write(JSON.stringify(req) + "\n");
    });
  }

  async call(method: string, params?: unknown): Promise<RpcResponse> {
    const id = this.nextId++;
    return this.send({ jsonrpc: "2.0", id, method, params });
  }

  async initialize() {
    const r = await this.call("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "biturbo-recall-eval", version: "0.1.0" },
    });
    if (r.error) throw new Error(`initialize failed: ${r.error.message}`);
    this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  }

  async callTool(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const r = await this.call("tools/call", { name, arguments: args });
    if (r.error) throw new Error(`${name} failed: ${r.error.message}`);
    return r.result;
  }

  close() {
    try { this.proc.kill(); } catch { /* ignore */ }
  }
}

function extractText(result: unknown): string {
  if (!result || typeof result !== "object") return "";
  const r = result as { content?: Array<{ type: string; text?: string }> };
  if (!Array.isArray(r.content)) return "";
  return r.content
    .filter((c) => c.type === "text" && typeof c.text === "string")
    .map((c) => c.text!)
    .join("\n");
}

function extractJson<T>(result: unknown): T {
  const text = extractText(result);
  if (!text) throw new Error("empty MCP text result");
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error(`MCP returned non-JSON text: ${text}`);
  }
}

function loadGolden(): GoldenFile {
  const file = arg("golden", "evals/recall-golden.json")!;
  return JSON.parse(readFileSync(resolve(process.cwd(), file), "utf8")) as GoldenFile;
}

async function seedGolden(client: McpClient, golden: GoldenFile, projectId: string): Promise<Map<string, string>> {
  await client.callTool("create_project", {
    id: projectId,
    name: `${golden.project.name} ${projectId}`,
    description: "Temporary recall eval project",
  });

  const aliases = new Map<string, string>();
  for (const mem of golden.memories) {
    const result = await client.callTool("remember", {
      content: mem.content,
      mem_type: mem.mem_type,
      project_id: projectId,
      tags: mem.tags ?? [],
      importance: mem.importance ?? 0.5,
      source_agent: "recall-eval",
      file_path: mem.file_path,
      start_line: mem.start_line,
      end_line: mem.end_line,
      language: mem.language,
      supersedes: mem.supersedes_alias ? aliases.get(mem.supersedes_alias) : undefined,
    });
    const json = extractJson<{ uid: string }>(result);
    aliases.set(mem.alias, json.uid);
  }
  return aliases;
}

type SearchHit = {
  uid?: string;
  score?: number;
  content?: string;
  mem_type?: string;
  tags?: string[];
  file_path?: string;
};

async function runCase(client: McpClient, projectId: string, aliases: Map<string, string>, test: GoldenCase) {
  const k = test.k ?? 5;
  const searchResult = await client.callTool("search", {
    query: test.query,
    project_id: projectId,
    k,
    mem_type: test.mem_type,
  });
  const hits = extractJson<SearchHit[]>(searchResult);

  const expectedUids = new Set(test.expected_aliases.map((a) => aliases.get(a)).filter(Boolean));
  const hitIndex = hits.findIndex((h) => h.uid && expectedUids.has(h.uid));

  const excludedUids = new Set(
    (test.must_exclude_aliases ?? []).map((alias) => aliases.get(alias)).filter(Boolean),
  );
  const excludedHit = hits.some((hit) => hit.uid && excludedUids.has(hit.uid));
  const recallResult = await client.callTool("recall_for_context", {
    query: test.query,
    project_id: projectId,
    k,
    mem_type: test.mem_type,
  });
  const context = extractText(recallResult);
  const missingTerms = (test.must_include ?? []).filter(
    (term) => !context.toLowerCase().includes(term.toLowerCase()),
  );

  return {
    name: test.name,
    hit: test.expect_empty ? hits.length === 0 : hitIndex >= 0,
    rank: hitIndex >= 0 ? hitIndex + 1 : null,
    missingTerms,
    excludedHit,
    top: hits.slice(0, k).map((h) => ({
      uid: h.uid,
      score: h.score,
      type: h.mem_type,
      path: h.file_path,
      preview: h.content?.slice(0, 80),
    })),
  };
}

async function main() {
  const golden = loadGolden();
  const bin = findBinary();
  console.log(`${COL.bold}biTurbo recall eval${COL.reset}`);
  console.log(`${COL.dim}binary:${COL.reset} ${bin}`);

  if (!existsSync(bin) && !arg("bin")) {
    console.error(`${COL.red}Could not locate biturbo-mcp. Run pnpm mcp:build or pass --bin=...${COL.reset}`);
    process.exit(2);
  }

  const projectId = `${golden.project.id_prefix}-${Date.now().toString(36)}`;
  const isolatedProjectId = `${projectId}-isolated`;
  const client = new McpClient(bin);
  const results: Awaited<ReturnType<typeof runCase>>[] = [];

  try {
    await client.initialize();
    const aliases = await seedGolden(client, golden, projectId);
    if (golden.isolation_probe) {
      await client.callTool("create_project", {
        id: isolatedProjectId,
        name: `${golden.project.name} Isolated ${projectId}`,
      });
      await client.callTool("remember", {
        ...golden.isolation_probe,
        project_id: isolatedProjectId,
      });
    }
    for (const c of golden.cases) results.push(await runCase(client, projectId, aliases, c));
    await client.callTool("delete_project", { project_id: projectId });
    if (golden.isolation_probe) {
      await client.callTool("delete_project", { project_id: isolatedProjectId });
    }
  } finally {
    client.close();
  }

  const hits = results.filter((r) => r.hit).length;
  const fullPass = results.filter(
    (r) => r.hit && r.missingTerms.length === 0 && !r.excludedHit,
  ).length;
  const recallAtK = hits / results.length;

  console.log("");
  for (const r of results) {
    const status = r.hit && r.missingTerms.length === 0 && !r.excludedHit
      ? `${COL.green}PASS${COL.reset}`
      : `${COL.red}FAIL${COL.reset}`;
    const rank = r.rank ? `rank=${r.rank}` : "not found";
    console.log(`  ${status} ${r.name} ${COL.dim}${rank}${COL.reset}`);
    if (r.missingTerms.length) console.log(`       missing in context: ${r.missingTerms.join(", ")}`);
    if (r.excludedHit) console.log("       stale or isolated result was returned");
    if (!r.hit || r.missingTerms.length || r.excludedHit) {
      console.log("       top hits:");
      for (const h of r.top) {
        const score = typeof h.score === "number" ? h.score.toFixed(4) : "n/a";
        console.log(`       - ${score} ${h.type ?? ""} ${h.path ?? ""} ${h.preview ?? ""}`);
      }
    }
  }

  console.log("");
  console.log(`${COL.bold}recall@k:${COL.reset} ${(recallAtK * 100).toFixed(1)}% (${hits}/${results.length})`);
  console.log(`${COL.bold}context pass:${COL.reset} ${fullPass}/${results.length}`);

  if (fullPass !== results.length) process.exit(1);
}

main().catch((e) => {
  console.error(`${COL.red}recall eval crashed:${COL.reset}`, e);
  process.exit(2);
});
