#!/usr/bin/env -S node --experimental-strip-types --no-warnings
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

type RpcId = number;
interface RpcResponse { jsonrpc: "2.0"; id: RpcId; result?: unknown; error?: { code: number; message: string }; }
interface RpcNotification { jsonrpc: "2.0"; method: string; params?: unknown; }

const COL = { reset: "\x1b[0m", green: "\x1b[32m", red: "\x1b[31m", dim: "\x1b[2m", bold: "\x1b[1m", yellow: "\x1b[33m", cyan: "\x1b[36m" };

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
      let parsed: RpcResponse | RpcNotification;
      try { parsed = JSON.parse(line); } catch { continue; }
      if ("id" in parsed && parsed.id !== undefined && this.pending.has(parsed.id as RpcId)) {
        this.pending.get(parsed.id as RpcId)!(parsed as RpcResponse);
        this.pending.delete(parsed.id as RpcId);
      }
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
    const r = await this.call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "search-test", version: "0.1" } });
    if (r.error) throw new Error(`initialize failed: ${r.error.message}`);
    this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  }

  async callTool(name: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const r = await this.call("tools/call", { name, arguments: args });
    if (r.error) throw new Error(`${name} failed: ${r.error.message}`);
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

type SearchResult = { uid: string; content: string; mem_type: string; score: number; tags?: string[]; file_path?: string | null };

const TEST_MEMORIES = [
  { alias: "auth_jwt", content: "Authentication uses JWT tokens with RS256 signing. Tokens expire after 24 hours. Refresh tokens are stored in httpOnly cookies.", mem_type: "decision", tags: ["auth", "jwt", "security"], importance: 0.9 },
  { alias: "auth_oauth", content: "OAuth2 flow uses Google as the identity provider. Callback URL is /api/auth/callback. We use the authorization code flow with PKCE.", mem_type: "pattern", tags: ["auth", "oauth", "google"], importance: 0.85 },
  { alias: "db_postgres", content: "Database is PostgreSQL 15 running on port 5432. Connection string uses SSL mode=require. Migrations are managed with Drizzle ORM.", mem_type: "fact", tags: ["database", "postgres", "drizzle"], importance: 0.8 },
  { alias: "db_redis", content: "Redis is used for session caching and rate limiting. Running on port 6379 with maxmemory 256mb and allkeys-lru eviction policy.", mem_type: "fact", tags: ["database", "redis", "cache"], importance: 0.75 },
  { alias: "api_rest", content: "REST API follows OpenAPI 3.0 spec. All endpoints return JSON with camelCase keys. Error responses use RFC 7807 Problem Details format.", mem_type: "decision", tags: ["api", "rest", "openapi"], importance: 0.8 },
  { alias: "api_graphql", content: "GraphQL endpoint at /graphql for frontend queries. Uses Apollo Server with automatic persisted queries. Schema-first approach with code generation.", mem_type: "fact", tags: ["api", "graphql", "apollo"], importance: 0.7 },
  { alias: "deploy_docker", content: "Deployment uses Docker Compose with 3 services: api, web, and worker. Images built with multi-stage Dockerfiles. Production uses Alpine base images.", mem_type: "decision", tags: ["deploy", "docker", "compose"], importance: 0.85 },
  { alias: "deploy_k8s", content: "Kubernetes deployment on GKE. 3 replicas for API, 2 for worker. Uses Horizontal Pod Autoscaler with CPU target 70%. Ingress via nginx-ingress controller.", mem_type: "fact", tags: ["deploy", "kubernetes", "gke"], importance: 0.8 },
  { alias: "testing_jest", content: "Unit tests use Jest with ts-jest preset. Coverage threshold is 80%. Integration tests use testcontainers for real PostgreSQL and Redis.", mem_type: "decision", tags: ["testing", "jest", "coverage"], importance: 0.75 },
  { alias: "testing_e2e", content: "E2E tests use Playwright. Tests run in headless Chromium. CI runs tests against preview deployments using Vercel deployment URLs.", mem_type: "fact", tags: ["testing", "playwright", "e2e"], importance: 0.7 },
  { alias: "pref_concise", content: "User preference: prefer concise, direct answers. Avoid long preambles unless depth is explicitly requested. Code examples over prose explanations.", mem_type: "preference", tags: ["style", "brevity"], importance: 0.9 },
  { alias: "pref_typescript", content: "User strongly prefers TypeScript over JavaScript. Strict mode enabled, no implicit any. Prefers interfaces over type aliases for object shapes.", mem_type: "preference", tags: ["typescript", "style"], importance: 0.85 },
  { alias: "code_middleware", content: "Express middleware stack order: cors, helmet, rateLimit, json parser, auth middleware, route handlers. Error handler is last.", mem_type: "pattern", tags: ["express", "middleware", "code"], importance: 0.8 },
  { alias: "code_error_handling", content: "Error handling pattern: all async route handlers wrapped in catchAsync(). Custom AppError class with statusCode and isOperational flag. Global error handler logs and formats response.", mem_type: "pattern", tags: ["error-handling", "express", "code"], importance: 0.85 },
  { alias: "episode_outage", content: "2024-03-15: Production outage lasted 2 hours. Root cause was Redis connection pool exhaustion under high load. Fix: increased max connections from 10 to 50 and added circuit breaker.", mem_type: "episode", tags: ["incident", "redis", "outage"], importance: 0.95 },
  { alias: "episode_migration", content: "2024-04-02: Database migration from MongoDB to PostgreSQL completed. 2.3M documents migrated. Used pgloader for bulk transfer, custom scripts for schema transformation.", mem_type: "episode", tags: ["migration", "postgres", "mongodb"], importance: 0.9 },
  { alias: "reflection_arch", content: "Reflection: monolith-to-microservices split was premature. The team of 3 developers spent more time on infrastructure than features. Should have stayed monolith longer.", mem_type: "reflection", tags: ["architecture", "lessons"], importance: 0.88 },
  { alias: "reflection_testing", content: "Reflection: investing in E2E tests early paid off. Caught 3 critical bugs before production that unit tests missed. Integration tests are the sweet spot for this project size.", mem_type: "reflection", tags: ["testing", "lessons"], importance: 0.82 },
  { alias: "perf_caching", content: "Performance optimization: added Redis caching for user profile lookups. Reduced p95 latency from 120ms to 15ms. Cache TTL is 5 minutes with write-through invalidation.", mem_type: "decision", tags: ["performance", "redis", "caching"], importance: 0.85 },
  { alias: "perf_indexes", content: "Performance optimization: added composite index on (user_id, created_at) for the activities table. Query time dropped from 2.3s to 12ms. Also added partial index for active records only.", mem_type: "decision", tags: ["performance", "postgres", "indexes"], importance: 0.83 },
];

const TEST_QUERIES: Array<{
  name: string;
  query: string;
  expectedTop?: string[];
  shouldNotContain?: string[];
  k: number;
}> = [
  { name: "exact: JWT auth", query: "JWT authentication tokens", expectedTop: ["auth_jwt", "auth_oauth"], k: 5 },
  { name: "semantic: how do users log in", query: "how does user login and session management work", expectedTop: ["auth_jwt", "auth_oauth"], k: 5 },
  { name: "multi: database setup", query: "what databases are we using and how are they configured", expectedTop: ["db_postgres", "db_redis"], k: 5 },
  { name: "preference: how should I respond", query: "how should I format my answers and what style does the user prefer", expectedTop: ["pref_concise", "pref_typescript"], k: 5 },
  { name: "episode: what went wrong in production", query: "production outage incident what happened", expectedTop: ["episode_outage"], k: 5 },
  { name: "pattern: error handling", query: "how do we handle errors in Express routes", expectedTop: ["code_error_handling", "code_middleware"], k: 5 },
  { name: "deploy: how is the app deployed", query: "deployment infrastructure docker kubernetes", expectedTop: ["deploy_docker", "deploy_k8s"], k: 5 },
  { name: "perf: what did we optimize", query: "performance improvements latency optimization", expectedTop: ["perf_caching", "perf_indexes"], k: 5 },
  { name: "reflection: what did we learn", query: "what lessons did we learn from past decisions", expectedTop: ["reflection_arch", "reflection_testing"], k: 5 },
  { name: "ambiguous: redis", query: "redis", expectedTop: ["db_redis", "episode_outage", "perf_caching"], k: 5 },
  { name: "cross: testing infrastructure", query: "testing setup jest playwright testcontainers", expectedTop: ["testing_jest", "testing_e2e"], k: 5 },
  { name: "negative: python django", query: "python django flask framework", shouldNotContain: ["auth_jwt", "db_postgres", "deploy_docker"], k: 5 },
];

async function main() {
  const bin = process.argv[2] ?? resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp.exe");
  if (!existsSync(bin)) { console.error(`${COL.red}Binary not found: ${bin}${COL.reset}`); process.exit(1); }

  console.log(`${COL.bold}${COL.cyan}=== biTurbo Search & Recall Quality Test ===${COL.reset}`);
  console.log(`${COL.dim}binary: ${bin}${COL.reset}\n`);

  const client = new McpClient(bin);
  const projectId = `search-test-${Date.now().toString(36)}`;

  try {
    await client.initialize();

    await client.callTool("create_project", { id: projectId, name: "Search Test", description: "Automated search quality test" });
    console.log(`${COL.dim}Created project: ${projectId}${COL.reset}\n`);

    const uidMap = new Map<string, string>();
    for (const mem of TEST_MEMORIES) {
      const result = await client.callTool("remember", {
        content: mem.content,
        mem_type: mem.mem_type,
        project_id: projectId,
        tags: mem.tags,
        importance: mem.importance,
        source_agent: "search-test",
      });
      const json = extractJson<{ uid: string }>(result);
      uidMap.set(mem.alias, json.uid);
    }
    console.log(`${COL.green}Seeded ${TEST_MEMORIES.length} test memories${COL.reset}\n`);

    let totalPass = 0;
    let totalFail = 0;
    const failures: string[] = [];

    function aliasOf(uid: string): string {
      return [...uidMap.entries()].find(([_, u]) => u === uid)?.[0] ?? uid.slice(0, 8);
    }

    for (const test of TEST_QUERIES) {
      console.log(`${COL.bold}${test.name}${COL.reset}`);
      console.log(`  Query: "${test.query}"`);

      const searchResult = await client.callTool("search", { query: test.query, project_id: projectId, k: test.k });
      const hits = extractJson<SearchResult[]>(searchResult);

      console.log(`  ${COL.dim}search() returned ${hits.length} hits:${COL.reset}`);
      for (let i = 0; i < Math.min(hits.length, 5); i++) {
        const h = hits[i];
        const alias = aliasOf(h.uid);
        const isExpected = test.expectedTop?.includes(alias);
        const isUnwanted = test.shouldNotContain?.includes(alias);
        const marker = isExpected ? `${COL.green}*${COL.reset}` : isUnwanted ? `${COL.red}X${COL.reset}` : " ";
        console.log(`    ${marker} [${i + 1}] ${alias.padEnd(22)} score=${h.score.toFixed(4)}  ${h.mem_type}  tags=${(h.tags ?? []).join(",")}`);
      }

      let pass = true;
      if (test.expectedTop) {
        const topAliases = hits.slice(0, test.expectedTop.length).map(h => aliasOf(h.uid));
        const allFound = test.expectedTop.every(alias => topAliases.includes(alias));
        if (!allFound) {
          pass = false;
          failures.push(`${test.name}: expected [${test.expectedTop.join(", ")}] in top ${test.expectedTop.length}, got [${topAliases.join(", ")}]`);
        }
      }
      if (test.shouldNotContain) {
        const topAliases = hits.slice(0, 3).map(h => aliasOf(h.uid));
        const anyFound = test.shouldNotContain.some(alias => topAliases.includes(alias));
        if (anyFound) {
          pass = false;
          failures.push(`${test.name}: should NOT contain [${test.shouldNotContain.join(", ")}] in top 3`);
        }
      }

      if (pass) {
        console.log(`  ${COL.green}PASS${COL.reset}\n`);
        totalPass++;
      } else {
        console.log(`  ${COL.red}FAIL${COL.reset}\n`);
        totalFail++;
      }
    }

    // Reranking quality tests
    console.log(`\n${COL.bold}${COL.cyan}=== Reranking Quality Tests ===${COL.reset}\n`);

    // importance boost
    {
      console.log(`${COL.bold}importance: high-importance should rank higher${COL.reset}`);
      const r = await client.callTool("search", { query: "authentication security tokens", project_id: projectId, k: 5 });
      const hits = extractJson<SearchResult[]>(r);
      const jwtIdx = hits.findIndex(h => h.uid === uidMap.get("auth_jwt"));
      const oauthIdx = hits.findIndex(h => h.uid === uidMap.get("auth_oauth"));
      if (jwtIdx >= 0 && oauthIdx >= 0 && jwtIdx < oauthIdx) {
        console.log(`  ${COL.green}PASS${COL.reset} - auth_jwt (imp=0.9) at rank ${jwtIdx + 1}, auth_oauth (imp=0.85) at rank ${oauthIdx + 1}\n`);
        totalPass++;
      } else {
        console.log(`  ${COL.red}FAIL${COL.reset} - auth_jwt at ${jwtIdx + 1}, auth_oauth at ${oauthIdx + 1}\n`);
        totalFail++;
        failures.push("importance: auth_jwt (0.9) should rank above auth_oauth (0.85)");
      }
    }

    // exact match boost
    {
      console.log(`${COL.bold}exact-match: query terms in content should boost rank${COL.reset}`);
      const r = await client.callTool("search", { query: "Drizzle ORM migrations", project_id: projectId, k: 5 });
      const hits = extractJson<SearchResult[]>(r);
      const dbPgIdx = hits.findIndex(h => h.uid === uidMap.get("db_postgres"));
      if (dbPgIdx === 0) {
        console.log(`  ${COL.green}PASS${COL.reset} - db_postgres (contains 'Drizzle' and 'migrations') at rank 1\n`);
        totalPass++;
      } else {
        console.log(`  ${COL.red}FAIL${COL.reset} - expected db_postgres at rank 1, got ${aliasOf(hits[0]?.uid)} at rank 1 (db_postgres at ${dbPgIdx + 1})\n`);
        totalFail++;
        failures.push("exact-match: db_postgres should be rank 1 for 'Drizzle ORM migrations'");
      }
    }

    // tag match boost
    {
      console.log(`${COL.bold}tag-match: matching tags should boost rank${COL.reset}`);
      const r = await client.callTool("search", { query: "performance optimization", project_id: projectId, k: 5 });
      const hits = extractJson<SearchResult[]>(r);
      const topPerf = hits.slice(0, 2).every(h => h.tags?.includes("performance"));
      if (topPerf) {
        console.log(`  ${COL.green}PASS${COL.reset} - top 2 results both tagged with 'performance'\n`);
        totalPass++;
      } else {
        console.log(`  ${COL.red}FAIL${COL.reset} - top 2: ${hits.slice(0, 2).map(h => `${aliasOf(h.uid)}(tags=${(h.tags ?? []).join(",")})`).join(", ")}\n`);
        totalFail++;
        failures.push("tag-match: performance-tagged memories should rank in top 2");
      }
    }

    // mem_type filter
    {
      console.log(`${COL.bold}type-filter: mem_type filter should restrict results${COL.reset}`);
      const r = await client.callTool("search", { query: "what did we decide", project_id: projectId, k: 5, mem_type: "decision" });
      const hits = extractJson<SearchResult[]>(r);
      const allDecisions = hits.every(h => h.mem_type === "decision");
      if (allDecisions && hits.length > 0) {
        console.log(`  ${COL.green}PASS${COL.reset} - all ${hits.length} results are type=decision\n`);
        totalPass++;
      } else {
        console.log(`  ${COL.red}FAIL${COL.reset} - got types: ${hits.map(h => h.mem_type).join(", ")}\n`);
        totalFail++;
        failures.push("type-filter: mem_type=decision should only return decisions");
      }
    }

    // Summary
    console.log(`\n${COL.bold}${COL.cyan}=== Summary ===${COL.reset}`);
    console.log(`  ${COL.green}${totalPass} passed${COL.reset} / ${COL.red}${totalFail} failed${COL.reset} / ${totalPass + totalFail} total`);
    if (failures.length) {
      console.log(`\n${COL.red}Failures:${COL.reset}`);
      failures.forEach(f => console.log(`  - ${f}`));
    }

    await client.callTool("delete_project", { project_id: projectId });
    console.log(`\n${COL.dim}Cleaned up test project${COL.reset}`);

    process.exit(totalFail > 0 ? 1 : 0);
  } finally {
    client.close();
  }
}

main().catch((e) => { console.error(`${COL.red}Crashed: ${e}${COL.reset}`); process.exit(2); });
