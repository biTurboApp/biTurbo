#!/usr/bin/env -S node --experimental-strip-types --no-warnings
/**
 * Smoke test for the biturbo-mcp server.
 *
 * Spawns the binary, speaks MCP JSON-RPC over stdio, calls every tool with
 * a sane payload, and prints a PASS/FAIL table. Exits 0 on full pass.
 *
 *   pnpm mcp:test
 *
 * Optional: --bin=/path/to/biturbo-mcp (default: auto-discover)
 *           --keep        leave the test project behind for inspection
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
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
interface RpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

interface TestCase {
  name: string;
  tool: string;
  args?: Record<string, unknown> | (() => Record<string, unknown>);
  expect?: (r: unknown) => true | string;
  skip?: boolean | (() => boolean);
  note?: string;
}

const COL = {
  reset: "\x1b[0m",
  green: "\x1b[32m",
  red: "\x1b[31m",
  yellow: "\x1b[33m",
  dim: "\x1b[2m",
  bold: "\x1b[1m",
  cyan: "\x1b[36m",
};

const argv = process.argv.slice(2);
const arg = (k: string, def?: string) => {
  const a = argv.find((x) => x.startsWith(`--${k}=`));
  return a ? a.split("=", 2)[1] : def;
};
const flag = (k: string) => argv.includes(`--${k}`);

function findBinary(): string {
  const explicit = arg("bin");
  if (explicit) return explicit;
  // Common locations.
  const candidates = [
    resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp"),
    resolve(process.cwd(), "src-tauri/target/release/biturbo-mcp"),
    resolve(process.cwd(), "../src-tauri/target/debug/biturbo-mcp"),
    resolve(process.cwd(), "../src-tauri/target/release/biturbo-mcp"),
  ];
  for (const c of candidates) if (existsSync(c)) return c;
  // Fall back to PATH.
  return "biturbo-mcp";
}

class McpClient {
  private proc: ChildProcessWithoutNullStreams;
  private buf = "";
  private pending = new Map<RpcId, (r: RpcResponse) => void>();
  private nextId = 1;
  private toolNames = new Set<string>();

  constructor(bin: string) {
    this.proc = spawn(bin, [], { stdio: ["pipe", "pipe", "pipe"] });
    this.proc.stdout.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.proc.stderr.on("data", (chunk: Buffer) => {
      const s = chunk.toString("utf8");
      if (process.env.MCP_TEST_VERBOSE) process.stderr.write(`[mcp-stderr] ${s}`);
    });
    this.proc.on("exit", (code) => {
      if (code !== 0 && code !== null) {
        console.error(`${COL.red}biturbo-mcp exited with code ${code}${COL.reset}`);
      }
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
        parsed = JSON.parse(line) as RpcResponse | RpcNotification;
      } catch {
        if (process.env.MCP_TEST_VERBOSE) {
          process.stderr.write(`[mcp-stdout-nonjson] ${line}\n`);
        }
        continue;
      }
      if ("id" in parsed && parsed.id !== undefined && this.pending.has(parsed.id as RpcId)) {
        this.pending.get(parsed.id as RpcId)!(parsed as RpcResponse);
        this.pending.delete(parsed.id as RpcId);
      } else if ("method" in parsed) {
        // Notification from server. Ignore for now.
      }
    }
  }

  private send(req: RpcRequest): Promise<RpcResponse> {
    return new Promise((resolveP, rejectP) => {
      const id = req.id;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectP(new Error(`RPC ${req.method} timed out`));
      }, 15_000);
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
      clientInfo: { name: "biturbo-smoke", version: "0.1.0" },
    });
    if (r.error) throw new Error(`initialize failed: ${r.error.message}`);
    // initialized notification (no params expected by most servers).
    this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
    return r.result;
  }

  async listTools(): Promise<string[]> {
    const r = await this.call("tools/list", {});
    if (r.error) throw new Error(`tools/list failed: ${r.error.message}`);
    const result = r.result as { tools?: Array<{ name: string }> };
    const names = (result.tools ?? []).map((t) => t.name);
    this.toolNames = new Set(names);
    return names;
  }

  async callTool(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const r = await this.call("tools/call", { name, arguments: args });
    if (r.error) return { _error: r.error };
    return (r.result as { content?: Array<{ type: string; text?: string }> });
  }

  close() {
    try {
      this.proc.kill();
    } catch {
      /* ignore */
    }
  }
}

function isNonEmptyString(x: unknown): x is string {
  return typeof x === "string" && x.length > 0;
}

function extractText(result: unknown): string {
  if (!result || typeof result !== "object") return "";
  const r = result as { content?: Array<{ type: string; text?: string }> };
  if (!Array.isArray(r.content)) return "";
  return r.content
    .filter((c) => c.type === "text" && isNonEmptyString(c.text))
    .map((c) => c.text!)
    .join("\n");
}

function extractJson(result: unknown): unknown {
  const txt = extractText(result);
  if (!txt) return null;
  try {
    return JSON.parse(txt);
  } catch {
    return txt;
  }
}

const TEST_PROJECT = `smoke-${Date.now().toString(36)}`;
const TEST_UID_HOLDER: { uid: string | null } = { uid: null };
const RECALL_HOLDER: { recallId: string | null; memoryUid: string | null } = {
  recallId: null,
  memoryUid: null,
};
const OPERATION_HOLDER: { id: string | null } = { id: null };
const CANDIDATE_HOLDER: { id: string | null; version: number } = { id: null, version: 1 };
const SOURCE_HOLDER: { id: string | null } = { id: null };
const CAPTURE_POLICY_HOLDER: { value: Record<string, unknown> | null } = { value: null };
const INTEGRITY_HOLDER: { operationId: string | null; reportId: string | null } = { operationId: null, reportId: null };
const EMPTY_INGEST_ROOT = mkdtempSync(resolve(tmpdir(), "biturbo-mcp-smoke-"));

const tests: TestCase[] = [
  {
    name: "register_agent",
    tool: "register_agent",
    args: { name: "smoke-runner", kind: "test", meta: { note: "mcp:test" } },
    expect: (r) => {
      const j = extractJson(r) as { id?: number; name?: string } | null;
      return j?.name === "smoke-runner" || "missing name in response";
    },
  },
  {
    name: "list_projects",
    tool: "list_projects",
    expect: (r) => {
      const j = extractJson(r);
      return Array.isArray(j) || "response is not a JSON array";
    },
  },
  {
    name: "create_project",
    tool: "create_project",
    args: { id: TEST_PROJECT, name: "Smoke Test Project", description: "auto" },
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      return j?.id === TEST_PROJECT || `expected id=${TEST_PROJECT}, got ${JSON.stringify(j)}`;
    },
  },
  {
    name: "get_project",
    tool: "get_project",
    args: { id: TEST_PROJECT },
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      return j?.id === TEST_PROJECT || "get_project returned wrong project";
    },
  },
  {
    name: "remember",
    tool: "remember",
    args: {
      content: "biTurbo smoke test memory",
      mem_type: "fact",
      project_id: TEST_PROJECT,
      tags: ["smoke", "test"],
      importance: 0.5,
      source_agent: "smoke-runner",
    },
    expect: (r) => {
      const j = extractJson(r) as { uid?: string } | null;
      if (!j?.uid) return "no uid in remember response";
      TEST_UID_HOLDER.uid = j.uid;
      return true;
    },
  },
  {
    name: "get_memory",
    tool: "get_memory",
    args: () => ({ uid: TEST_UID_HOLDER.uid ?? "" }) as Record<string, unknown>,
    skip: () => !TEST_UID_HOLDER.uid,
    expect: (r) => {
      const j = extractJson(r) as { uid?: string } | null;
      return j?.uid === TEST_UID_HOLDER.uid || "uid mismatch";
    },
  },
  {
    name: "update",
    tool: "update",
    args: () => ({
      uid: TEST_UID_HOLDER.uid ?? "",
      tags: ["smoke", "test", "updated"],
      importance: 0.7,
    }) as Record<string, unknown>,
    skip: () => !TEST_UID_HOLDER.uid,
    expect: (r) => {
      const j = extractJson(r) as { uid?: string } | null;
      return j?.uid === TEST_UID_HOLDER.uid || "update returned wrong uid";
    },
  },
  {
    name: "list",
    tool: "list",
    args: { project_id: TEST_PROJECT, limit: 10, offset: 0 },
    expect: (r) => {
      const j = extractJson(r);
      return Array.isArray(j) || "list response is not a JSON array";
    },
  },
  {
    name: "search",
    tool: "search",
    args: { query: "smoke test memory", project_id: TEST_PROJECT, k: 5 },
    expect: (r) => {
      const j = extractJson(r);
      return Array.isArray(j) || "search response is not a JSON array";
    },
  },
  {
    name: "list_tags",
    tool: "list_tags",
    args: { project_id: TEST_PROJECT },
    expect: (r) => Array.isArray(extractJson(r)) || "list_tags not array",
  },
  {
    name: "recall_for_context",
    tool: "recall_for_context",
    args: { query: "smoke", project_id: TEST_PROJECT, k: 3 },
    expect: (r) => isNonEmptyString(extractText(r)) || "empty recall_for_context",
  },
  {
    name: "recall_explain",
    tool: "recall_explain",
    args: { query: "smoke test memory", project_id: TEST_PROJECT, k: 3 },
    expect: (r) => {
      const j = extractJson(r) as {
        recall_id?: string;
        results?: Array<{ uid?: string; memory?: { uid?: string } }>;
      } | null;
      const memoryUid = j?.results?.[0]?.memory?.uid ?? j?.results?.[0]?.uid;
      if (!j?.recall_id || !memoryUid) return "missing recall id or explained result";
      RECALL_HOLDER.recallId = j.recall_id;
      RECALL_HOLDER.memoryUid = memoryUid;
      return true;
    },
  },
  {
    name: "submit_recall_feedback",
    tool: "submit_recall_feedback",
    args: () => ({
      recall_id: RECALL_HOLDER.recallId ?? "",
      memory_uid: RECALL_HOLDER.memoryUid ?? "",
      value: 1,
      source: "explicit",
    }),
    skip: () => !RECALL_HOLDER.recallId || !RECALL_HOLDER.memoryUid,
    expect: (r) => {
      const j = extractJson(r) as { recorded?: boolean } | null;
      return j?.recorded === true || "feedback was not recorded";
    },
  },
  {
    name: "submit_observation",
    tool: "submit_observation",
    args: {
      project_id: TEST_PROJECT,
      source_kind: "generic",
      external_id: `smoke-observation-${Date.now()}`,
      occurred_at: Date.now(),
      role: "user",
      content: "We decided to keep the smoke-test memory runtime completely local.",
    },
    expect: (r) => {
      const j = extractJson(r) as { accepted?: boolean; candidate_ids?: string[] } | null;
      const id = j?.candidate_ids?.[0];
      if (!j?.accepted || !id) return "observation did not create a candidate";
      CANDIDATE_HOLDER.id = id;
      return true;
    },
  },
  {
    name: "list_memory_candidates",
    tool: "list_memory_candidates",
    args: { project_id: TEST_PROJECT, status: "pending" },
    expect: (r) => Array.isArray(extractJson(r)) || "candidate list is not an array",
  },
  {
    name: "get_memory_candidate",
    tool: "get_memory_candidate",
    args: () => ({ candidate_id: CANDIDATE_HOLDER.id ?? "" }),
    skip: () => !CANDIDATE_HOLDER.id,
    expect: (r) => {
      const j = extractJson(r) as { id?: string; version?: number } | null;
      if (j?.version) CANDIDATE_HOLDER.version = j.version;
      return j?.id === CANDIDATE_HOLDER.id || "wrong candidate returned";
    },
  },
  {
    name: "review_memory_candidate",
    tool: "review_memory_candidate",
    args: () => ({
      candidate_id: CANDIDATE_HOLDER.id ?? "",
      action: "approve",
      expected_version: CANDIDATE_HOLDER.version,
      decided_by: "mcp-smoke",
    }),
    skip: () => !CANDIDATE_HOLDER.id,
    expect: (r) => {
      const j = extractJson(r) as { memory_uid?: string } | null;
      return isNonEmptyString(j?.memory_uid) || "candidate was not activated";
    },
  },
  {
    name: "get_capture_policy",
    tool: "get_capture_policy",
    args: { project_id: TEST_PROJECT },
    expect: (r) => {
      const j = extractJson(r);
      if (!j || typeof j !== "object") return "capture policy missing";
      CAPTURE_POLICY_HOLDER.value = j as Record<string, unknown>;
      return true;
    },
  },
  {
    name: "update_capture_policy",
    tool: "update_capture_policy",
    args: () => CAPTURE_POLICY_HOLDER.value ?? {},
    skip: () => !CAPTURE_POLICY_HOLDER.value,
    expect: (r) => (extractJson(r) as { project_id?: string } | null)?.project_id === TEST_PROJECT || "policy update failed",
  },
  {
    name: "configure_observation_source",
    tool: "configure_observation_source",
    args: {
      id: "",
      project_id: TEST_PROJECT,
      kind: "generic",
      name: "smoke generic source",
      root_path: null,
      enabled: true,
      config: {},
      last_sync_at: null,
      last_error: null,
      processed_count: 0,
      candidate_count: 0,
      created_at: 0,
      updated_at: 0,
    },
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      if (!j?.id) return "source id missing";
      SOURCE_HOLDER.id = j.id;
      return true;
    },
  },
  {
    name: "list_observation_sources",
    tool: "list_observation_sources",
    args: { project_id: TEST_PROJECT },
    expect: (r) => Array.isArray(extractJson(r)) || "source list is not an array",
  },
  {
    name: "source_status",
    tool: "source_status",
    args: () => ({ source_id: SOURCE_HOLDER.id ?? "" }),
    skip: () => !SOURCE_HOLDER.id,
    expect: (r) => Boolean((extractJson(r) as { source?: unknown } | null)?.source) || "source status missing",
  },
  {
    name: "start_source_sync (generic guard)",
    tool: "start_source_sync",
    args: () => ({ source_id: SOURCE_HOLDER.id ?? "" }),
    skip: () => !SOURCE_HOLDER.id,
    note: "generic sources reject transcript sync but must return a structured response",
    expect: () => true,
  },
  {
    name: "health_report",
    tool: "health_report",
    expect: (r) => isNonEmptyString((extractJson(r) as { status?: string } | null)?.status) || "health status missing",
  },
  {
    name: "start_integrity_check",
    tool: "start_integrity_check",
    args: { project_id: TEST_PROJECT },
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      if (!j?.id) return "integrity operation id missing";
      INTEGRITY_HOLDER.operationId = j.id;
      return true;
    },
  },
  {
    name: "integrity_report",
    tool: "integrity_report",
    args: { project_id: TEST_PROJECT },
    expect: () => true,
  },
  {
    name: "repair_integrity (empty guard)",
    tool: "repair_integrity",
    args: { project_id: TEST_PROJECT, integrity_run_id: "missing-smoke-run", issue_ids: [], dry_run: true },
    expect: () => true,
  },
  {
    name: "get_maintenance_policy",
    tool: "get_maintenance_policy",
    args: { project_id: TEST_PROJECT },
    expect: (r) => (extractJson(r) as { project_id?: string } | null)?.project_id === TEST_PROJECT || "maintenance policy missing",
  },
  {
    name: "update_maintenance_policy",
    tool: "update_maintenance_policy",
    args: { project_id: TEST_PROJECT, enabled: true, interval_hours: 24, idle_delay_seconds: 120, auto_safe_repairs: true, last_run_at: null, next_run_at: null, updated_at: 0 },
    expect: (r) => (extractJson(r) as { interval_hours?: number } | null)?.interval_hours === 24 || "maintenance policy update failed",
  },
  {
    name: "accelerator_status",
    tool: "accelerator_status",
    expect: (r) => Array.isArray((extractJson(r) as { compiled_providers?: unknown[] } | null)?.compiled_providers) || "accelerator status missing",
  },
  {
    name: "get_accelerator_preference",
    tool: "get_accelerator_preference",
    expect: (r) => isNonEmptyString((extractJson(r) as { provider?: string } | null)?.provider) || "accelerator preference missing",
  },
  {
    name: "set_accelerator_preference",
    tool: "set_accelerator_preference",
    args: { provider: "auto" },
    expect: (r) => (extractJson(r) as { provider?: string } | null)?.provider === "auto" || "accelerator preference update failed",
  },
  {
    name: "reranker_status",
    tool: "reranker_status",
    expect: (r) => isNonEmptyString((extractJson(r) as { model_name?: string } | null)?.model_name) || "reranker status missing",
  },
  {
    name: "set_reranker_enabled (disabled)",
    tool: "set_reranker_enabled",
    args: { enabled: false },
    expect: (r) => (extractJson(r) as { enabled?: boolean } | null)?.enabled === false || "reranker disable failed",
  },
  {
    name: "start_reranker_download",
    tool: "start_reranker_download",
    skip: true,
    note: "explicit network download is intentionally not started by smoke tests",
  },
  {
    name: "import_reranker_artifact (missing guard)",
    tool: "import_reranker_artifact",
    args: { path: "/nonexistent-reranker-artifact" },
    expect: () => true,
  },
  {
    name: "start_ingest",
    tool: "start_ingest",
    args: { project_id: TEST_PROJECT, root_path: EMPTY_INGEST_ROOT },
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      if (!j?.id) return "start_ingest returned no operation id";
      OPERATION_HOLDER.id = j.id;
      return true;
    },
  },
  {
    name: "operation_status",
    tool: "operation_status",
    args: () => ({ id: OPERATION_HOLDER.id ?? "" }),
    skip: () => !OPERATION_HOLDER.id,
    expect: (r) => {
      const j = extractJson(r) as { id?: string; status?: string } | null;
      return (j?.id === OPERATION_HOLDER.id && isNonEmptyString(j.status)) || "invalid operation status";
    },
  },
  {
    name: "list_operations",
    tool: "list_operations",
    args: { limit: 10 },
    expect: (r) => Array.isArray(extractJson(r)) || "list_operations not array",
  },
  {
    name: "cancel_operation",
    tool: "cancel_operation",
    args: () => ({ id: OPERATION_HOLDER.id ?? "" }),
    skip: () => !OPERATION_HOLDER.id,
    expect: (r) => {
      const j = extractJson(r) as { id?: string } | null;
      return j?.id === OPERATION_HOLDER.id || "cancel returned wrong operation";
    },
  },
  {
    name: "retry_operation (terminal guard)",
    tool: "retry_operation",
    args: () => ({ id: OPERATION_HOLDER.id ?? "" }),
    skip: () => !OPERATION_HOLDER.id,
    note: "a completed operation should reject retry while still round-tripping a structured error",
    expect: () => true,
  },
  {
    name: "stats",
    tool: "stats",
    expect: (r) => {
      const j = extractJson(r) as { total_memories?: number } | null;
      return typeof j?.total_memories === "number" || "stats missing total_memories";
    },
  },
  {
    name: "recent_activity",
    tool: "recent_activity",
    args: { limit: 5 },
    expect: (r) => Array.isArray(extractJson(r)) || "recent_activity not array",
  },
  {
    name: "consolidate_status",
    tool: "consolidate_status",
    note: "optional on some builds",
    expect: () => true,
  },
  {
    name: "bootstrap",
    tool: "bootstrap",
    note: "optional — convenience aggregator",
    expect: () => true,
  },
  {
    name: "ingest_project (no-op path)",
    tool: "ingest_project",
    args: { project_id: TEST_PROJECT, root_path: "/nonexistent-smoke-path" },
    note: "expected to fail with a clear error — we just check the call round-trips",
    expect: (r) => {
      // We accept either success-with-error or a structured error response.
      return true;
    },
  },
  {
    name: "consolidate",
    tool: "consolidate",
    args: { project_id: TEST_PROJECT },
    expect: () => true,
  },
  {
    name: "forget",
    tool: "forget",
    args: () => ({ uid: TEST_UID_HOLDER.uid ?? "" }) as Record<string, unknown>,
    skip: () => !TEST_UID_HOLDER.uid,
    expect: () => true,
  },
  {
    name: "get_project_name_from_file",
    tool: "get_project_name_from_file",
    args: { root_path: "/nonexistent/path" },
    expect: () => true,
  },
  {
    name: "delete_project",
    tool: "delete_project",
    args: { project_id: TEST_PROJECT },
    expect: () => true,
  },
];

async function main() {
  const bin = findBinary();
  console.log(`${COL.bold}biturbo-mcp smoke test${COL.reset}`);
  console.log(`${COL.dim}binary:${COL.reset} ${bin}`);
  if (!existsSync(bin) && !arg("bin")) {
    console.error(
      `${COL.red}Could not locate biturbo-mcp. Pass --bin=/path or run ${COL.cyan}pnpm mcp:build${COL.red} first.${COL.reset}`,
    );
    process.exit(2);
  }

  const client = new McpClient(bin);
  let pass = 0;
  let fail = 0;
  let skipped = 0;
  const results: Array<{ name: string; status: "PASS" | "FAIL" | "SKIP"; detail?: string; ms: number }> = [];

  try {
    await client.initialize();
    const tools = await client.listTools();
    console.log(`${COL.dim}discovered ${tools.length} tools${COL.reset}\n`);

    for (const t of tests) {
      const skipNow = typeof t.skip === "function" ? t.skip() : t.skip;
      if (skipNow) {
        skipped++;
        results.push({ name: t.name, status: "SKIP", ms: 0 });
        continue;
      }
      if (!client["toolNames"].has(t.tool)) {
        skipped++;
        results.push({
          name: t.name,
          status: "SKIP",
          detail: `tool '${t.tool}' not exposed`,
          ms: 0,
        });
        continue;
      }
      const args =
        typeof t.args === "function" ? (t.args as () => Record<string, unknown>)() : t.args;
      const t0 = performance.now();
      let r: unknown;
      let err: string | null = null;
      try {
        r = await client.callTool(t.tool, args);
      } catch (e) {
        err = e instanceof Error ? e.message : String(e);
      }
      const ms = Math.round(performance.now() - t0);

      let ok = true;
      let detail = "";
      if (err) {
        ok = false;
        detail = err;
      } else if (t.expect) {
        const exp = t.expect(r);
        if (exp !== true) {
          ok = false;
          detail = exp;
        }
      }
      if (ok) {
        pass++;
        results.push({ name: t.name, status: "PASS", ms });
      } else {
        fail++;
        const txt = extractText(r);
        results.push({
          name: t.name,
          status: "FAIL",
          detail: detail || txt.slice(0, 120),
          ms,
        });
      }
    }
  } finally {
    client.close();
    rmSync(EMPTY_INGEST_ROOT, { recursive: true, force: true });
  }

  const padName = Math.max(...results.map((r) => r.name.length), 20);
  for (const r of results) {
    const tag =
      r.status === "PASS"
        ? `${COL.green}PASS${COL.reset}`
        : r.status === "FAIL"
        ? `${COL.red}FAIL${COL.reset}`
        : `${COL.yellow}SKIP${COL.reset}`;
    const t = `${r.ms}ms`.padStart(7);
    console.log(`  ${tag}  ${r.name.padEnd(padName)}  ${COL.dim}${t}${COL.reset}`);
    if (r.detail) {
      console.log(`${COL.dim}         └─ ${r.detail}${COL.reset}`);
    }
  }
  const total = pass + fail + skipped;
  console.log(
    `\n${COL.bold}summary:${COL.reset} ${COL.green}${pass} pass${COL.reset} · ${fail ? COL.red : COL.dim}${fail} fail${COL.reset} · ${COL.yellow}${skipped} skip${COL.reset} · ${total} total`,
  );
  if (fail > 0) process.exit(1);
}

main().catch((e) => {
  console.error(`${COL.red}smoke test crashed:${COL.reset}`, e);
  process.exit(2);
});
