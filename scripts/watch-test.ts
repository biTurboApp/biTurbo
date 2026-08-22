#!/usr/bin/env -S node --experimental-strip-types --no-warnings
/**
 * Test the watch feature: enable watch on a project, modify a file,
 * and verify that re-ingestion happens automatically.
 */
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync, rmSync, readdirSync } from "node:fs";
import { resolve, join } from "node:path";

type RpcId = number;
interface RpcResponse { jsonrpc: "2.0"; id: RpcId; result?: unknown; error?: { code: number; message: string }; }

const COL = { reset: "\x1b[0m", green: "\x1b[32m", red: "\x1b[31m", dim: "\x1b[2m", bold: "\x1b[1m", cyan: "\x1b[36m" };

class McpClient {
  private proc: ChildProcessWithoutNullStreams;
  private buf = "";
  private pending = new Map<RpcId, (r: RpcResponse) => void>();
  private nextId = 1;

  constructor(bin: string) {
    this.proc = spawn(bin, [], { stdio: ["pipe", "pipe", "pipe"] });
    this.proc.stdout.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.proc.stderr.on("data", (chunk: Buffer) => {
      if (process.env.VERBOSE) process.stderr.write(`${COL.dim}[stderr] ${chunk.toString("utf8").trim()}${COL.reset}\n`);
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
        const parsed = JSON.parse(line);
        if ("id" in parsed && parsed.id !== undefined && this.pending.has(parsed.id as RpcId)) {
          this.pending.get(parsed.id as RpcId)!(parsed as RpcResponse);
          this.pending.delete(parsed.id as RpcId);
        }
      } catch {}
    }
  }

  async call(method: string, params?: unknown): Promise<RpcResponse> {
    const id = this.nextId++;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => { this.pending.delete(id); rej(new Error(`timeout: ${method}`)); }, 60_000);
      this.pending.set(id, (r) => { clearTimeout(timer); res(r); });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async initialize() {
    const r = await this.call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "watch-test", version: "0.1" } });
    if (r.error) throw new Error(`initialize failed: ${r.error.message}`);
    this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  }

  async callTool(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const r = await this.call("tools/call", { name, arguments: args });
    if (r.error) throw new Error(`${name} failed: ${r.error.message}`);
    if (process.env.VERBOSE) {
      console.log(`${COL.dim}[MCP] ${name} → ${JSON.stringify(r.result).slice(0, 200)}${COL.reset}`);
    }
    return r.result;
  }

  close() { try { this.proc.kill(); } catch {} }
}

function extractText(result: unknown): string {
  if (!result || typeof result !== "object") return "";
  const r = result as { content?: Array<{ type: string; text?: string }> };
  if (!Array.isArray(r.content)) return "";
  return r.content.filter((c) => c.type === "text" && typeof c.text === "string").map((c) => c.text!).join("\n");
}

function extractJson<T>(result: unknown): T {
  return JSON.parse(extractText(result)) as T;
}

async function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function main() {
  const bin = process.argv[2] ?? resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp.exe");
  if (!existsSync(bin)) { console.error(`${COL.red}Binary not found: ${bin}${COL.reset}`); process.exit(1); }

  console.log(`${COL.bold}${COL.cyan}=== Watch Feature Test ===${COL.reset}\n`);

  const client = new McpClient(bin);
  const projectId = `watch-test-${Date.now().toString(36)}`;
  const testDir = resolve(process.cwd(), `test-watch-${Date.now().toString(36)}`);

  try {
    await client.initialize();

    // Create test directory with initial code file
    mkdirSync(testDir, { recursive: true });
    writeFileSync(join(testDir, "initial.rs"), "// Initial function\nfn hello() {\n    println!(\"Hello\");\n}\n");
    console.log(`${COL.dim}Created test directory: ${testDir}${COL.reset}`);

    // Create project
    const projectName = `Watch Test ${Date.now()}`;
    const createResult = await client.callTool("create_project", { id: projectId, name: projectName, root_path: testDir });
    const createText = extractText(createResult);
    if (createText.includes("error")) {
      console.error(`${COL.red}Failed to create project: ${createText}${COL.reset}`);
      process.exit(1);
    }
    console.log(`${COL.dim}Created project: ${projectId}${COL.reset}\n`);

    // Initial ingest
    console.log(`${COL.bold}Step 1: Initial ingest${COL.reset}`);
    const t1 = Date.now();
    await client.callTool("ingest_project", { project_id: projectId, root_path: testDir });
    const ingestTime = Date.now() - t1;
    console.log(`  Initial ingest completed in ${ingestTime}ms`);

    // Check initial stats via list
    const listBefore = extractJson<any[]>(
      await client.callTool("list", { project_id: projectId, limit: 100 })
    );
    const countBefore = listBefore.length;
    console.log(`  Memory count: ${countBefore}`);

    // Enable watch
    console.log(`\n${COL.bold}Step 2: Enable watch${COL.reset}`);
    await client.callTool("enable_watch", { project_id: projectId, root_path: testDir });
    console.log(`  Watch enabled`);

    // Check watch status
    const watchStatus = extractJson<{ enabled_projects: number; watching: string[] }>(
      await client.callTool("watch_status", {})
    );
    console.log(`  Watching ${watchStatus.enabled_projects} project(s): ${watchStatus.watching.join(", ")}`);

    // Modify file and wait for re-ingestion
    console.log(`\n${COL.bold}Step 3: Modify file and wait for auto-reingest${COL.reset}`);
    writeFileSync(join(testDir, "new-file.rs"), "// New function added after watch enabled\nfn world() {\n    println!(\"World\");\n}\n");
    console.log(`  Created new-file.rs`);
    
    console.log(`  Waiting 5 seconds for watcher to trigger re-ingestion...`);
    await sleep(5000);

    // Check if re-ingestion happened
    const listAfter = extractJson<any[]>(
      await client.callTool("list", { project_id: projectId, limit: 100 })
    );
    const countAfter = listAfter.length;
    console.log(`  Memory count after watch: ${countAfter}`);

    if (countAfter > countBefore) {
      console.log(`\n${COL.green}✓ SUCCESS: Watch triggered re-ingestion!${COL.reset}`);
      console.log(`  Memories increased from ${countBefore} to ${countAfter}`);
    } else {
      console.log(`\n${COL.red}✗ FAIL: Watch did not trigger re-ingestion${COL.reset}`);
      console.log(`  Memory count stayed at ${countBefore}`);
    }

    // Disable watch
    console.log(`\n${COL.bold}Step 4: Disable watch${COL.reset}`);
    await client.callTool("disable_watch", { project_id: projectId });
    const watchStatusAfter = extractJson<{ enabled_projects: number }>(
      await client.callTool("watch_status", {})
    );
    console.log(`  Watch disabled, now watching ${watchStatusAfter.enabled_projects} project(s)`);

    // Cleanup
    await client.callTool("delete_project", { project_id: projectId });
    rmSync(testDir, { recursive: true, force: true });
    console.log(`\n${COL.dim}Cleaned up${COL.reset}`);

    process.exit(countAfter > countBefore ? 0 : 1);
  } finally {
    client.close();
  }
}

main().catch((e) => { console.error(`${COL.red}Crashed: ${e}${COL.reset}`); process.exit(2); });
