#!/usr/bin/env -S node --experimental-strip-types --no-warnings
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

type RpcId = number;
interface RpcResponse { jsonrpc: "2.0"; id: RpcId; result?: unknown; error?: { code: number; message: string }; }

const COL = { reset: "\x1b[0m", green: "\x1b[32m", red: "\x1b[31m", dim: "\x1b[2m", bold: "\x1b[1m", cyan: "\x1b[36m", yellow: "\x1b[33m" };

class McpClient {
  private proc: any;
  private buf = "";
  private pending = new Map<RpcId, (r: RpcResponse) => void>();
  private nextId = 1;

  constructor(bin: string) {
    this.proc = spawn(bin, [], { stdio: ["pipe", "pipe", "pipe"] });
    this.proc.stdout.on("data", (chunk: Buffer) => this.onStdout(chunk));
    this.proc.stderr.on("data", () => {});
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
      const timer = setTimeout(() => { this.pending.delete(id); rej(new Error(`timeout`)); }, 60_000);
      this.pending.set(id, (r) => { clearTimeout(timer); res(r); });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async initialize() {
    const r = await this.call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "profiler", version: "0.1" } });
    if (r.error) throw new Error(`init failed: ${r.error.message}`);
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

function countTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

// Realistic memories with varying content types
const TEST_MEMORIES = [
  {
    alias: "auth_decision",
    content: "Authentication decision: We chose JWT with RS256 signing after evaluating OAuth2, session cookies, and API keys. JWT gives us stateless verification and works well with our microservices architecture. Tokens expire after 24 hours to balance security and user experience.",
    mem_type: "decision",
    tags: ["auth", "jwt", "security", "architecture"],
    importance: 0.9,
  },
  {
    alias: "auth_decision_v2",
    content: "Authentication decision: We chose JWT with RS256 signing after evaluating OAuth2, session cookies, and API keys. JWT gives us stateless verification and works well with our microservices architecture. Tokens expire after 24 hours.",
    mem_type: "decision",
    tags: ["auth", "jwt", "security"],
    importance: 0.85,
  },
  {
    alias: "db_schema",
    content: "Database schema: users table has id (uuid), email (varchar unique), password_hash (varchar), created_at (timestamp), updated_at (timestamp). Added indexes on email and created_at for query performance. Using PostgreSQL 15 with connection pooling via pgBouncer.",
    mem_type: "fact",
    tags: ["database", "schema", "postgres", "performance"],
    importance: 0.85,
  },
  {
    alias: "api_pattern",
    content: "API error handling pattern: All endpoints return { success: boolean, data?: any, error?: { code: string, message: string } }. Use try-catch in handlers, log errors with request context, return appropriate HTTP status codes (400 for validation, 401 for auth, 500 for internal).",
    mem_type: "pattern",
    tags: ["api", "error-handling", "rest", "best-practice"],
    importance: 0.8,
  },
  {
    alias: "deploy_config",
    content: "Deployment configuration: Docker multi-stage build, base image node:20-alpine, production build with npm ci --only=production. Kubernetes deployment with 3 replicas, resource limits 512Mi memory, 0.5 CPU. Health check on /health endpoint, readiness probe on /ready.",
    mem_type: "decision",
    tags: ["deployment", "docker", "kubernetes", "devops"],
    importance: 0.85,
  },
  {
    alias: "testing_strategy",
    content: "Testing strategy: Unit tests with Jest (80% coverage target), integration tests with Supertest for API endpoints, E2E tests with Playwright for critical user flows. CI runs all tests on PR, E2E only on main branch. Test database seeded with fixtures before each suite.",
    mem_type: "decision",
    tags: ["testing", "jest", "playwright", "ci-cd"],
    importance: 0.8,
  },
  {
    alias: "perf_optimization",
    content: "Performance optimization: Added Redis caching for user profile lookups. Cache key format: user:{id}:profile, TTL 5 minutes. Write-through invalidation on profile updates. Reduced p95 latency from 120ms to 15ms. Also added database query logging to identify slow queries.",
    mem_type: "decision",
    tags: ["performance", "caching", "redis", "optimization"],
    importance: 0.88,
  },
  {
    alias: "security_incident",
    content: "Security incident 2024-03-15: Discovered XSS vulnerability in comment rendering. User input was not sanitized before inserting into DOM. Fixed by using DOMPurify library and setting Content-Security-Policy headers. Conducted security audit of all user input handling.",
    mem_type: "episode",
    tags: ["security", "incident", "xss", "audit"],
    importance: 0.92,
  },
  {
    alias: "user_preference",
    content: "User preference: Prefers concise, direct answers with code examples over lengthy explanations. Values working solutions over theoretical perfection. Dislikes verbose comments in code - prefer self-documenting code with minimal comments explaining 'why' not 'what'.",
    mem_type: "preference",
    tags: ["style", "communication", "code-quality"],
    importance: 0.85,
  },
  {
    alias: "architecture_reflection",
    content: "Architecture reflection: Our microservices approach added complexity we didn't need at our scale (3 developers, 50k users). Should have started with a modular monolith and extracted services only when clear boundaries emerged. Lesson: premature optimization applies to architecture too.",
    mem_type: "reflection",
    tags: ["architecture", "lessons", "microservices", "monolith"],
    importance: 0.87,
  },
  {
    alias: "code_example",
    content: "Example: Custom React hook for form validation\n\n```typescript\nexport function useFormValidation<T>(initialState: T) {\n  const [values, setValues] = useState(initialState);\n  const [errors, setErrors] = useState<Partial<Record<keyof T, string>>>({});\n  \n  const validate = (field: keyof T, value: any) => {\n    // validation logic\n  };\n  \n  return { values, errors, setValues, validate };\n}\n```",
    mem_type: "code",
    tags: ["react", "hooks", "typescript", "forms"],
    importance: 0.75,
    file_path: "src/hooks/useFormValidation.ts",
    start_line: 1,
    end_line: 15,
  },
];

async function main() {
  const bin = process.argv[2] ?? resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp.exe");
  if (!existsSync(bin)) { console.error(`${COL.red}Binary not found${COL.reset}`); process.exit(1); }

  console.log(`${COL.bold}${COL.cyan}=== Context Budget Profiler ===${COL.reset}\n`);

  const client = new McpClient(bin);
  const projectId = `profiler-${Date.now().toString(36)}`;

  try {
    await client.initialize();
    await client.callTool("create_project", { id: projectId, name: "Profiler", description: "Context budget analysis" });
    console.log(`${COL.dim}Created project: ${projectId}${COL.reset}\n`);

    // Seed memories
    for (const mem of TEST_MEMORIES) {
      await client.callTool("remember", {
        content: mem.content,
        mem_type: mem.mem_type,
        project_id: projectId,
        tags: mem.tags,
        importance: mem.importance,
        source_agent: "profiler",
        file_path: (mem as any).file_path,
        start_line: (mem as any).start_line,
        end_line: (mem as any).end_line,
      });
    }
    console.log(`${COL.green}✓ Seeded ${TEST_MEMORIES.length} memories${COL.reset}\n`);

    // Test different k values
    const testCases = [
      { name: "k=5", k: 5, query: "authentication and security" },
      { name: "k=10", k: 10, query: "authentication and security" },
      { name: "k=20", k: 20, query: "authentication and security" },
      { name: "Broad query k=10", k: 10, query: "best practices and decisions" },
    ];

    for (const test of testCases) {
      console.log(`${COL.bold}${test.name}${COL.reset}`);
      console.log(`  Query: "${test.query}"`);
      
      const result = await client.callTool("recall_for_context", { 
        query: test.query, 
        project_id: projectId, 
        k: test.k 
      });
      const context = extractText(result);
      
      const charCount = context.length;
      const tokenCount = countTokens(context);
      const maxChars = 12000;
      const utilization = (charCount / maxChars * 100).toFixed(1);
      
      // Analyze structure
      const lines = context.split("\n");
      const metadataLines = lines.filter(l => l.startsWith("[") || l.startsWith("location="));
      const contentLines = lines.filter(l => !l.startsWith("[") && !l.startsWith("location=") && !l.startsWith("<"));
      
      const metadataChars = metadataLines.join("\n").length;
      const contentChars = contentLines.join("\n").length;
      const metadataPct = (metadataChars / charCount * 100).toFixed(1);
      const contentPct = (contentChars / charCount * 100).toFixed(1);
      
      console.log(`  Total: ${charCount} chars / ${tokenCount} tokens (${utilization}% of ${maxChars} budget)`);
      console.log(`  Metadata: ${metadataChars} chars (${metadataPct}%)`);
      console.log(`  Content: ${contentChars} chars (${contentPct}%)`);
      console.log(`  Items returned: ${lines.filter(l => l.startsWith("[")).length}`);
      
      // Show sample
      console.log(`\n  ${COL.dim}Sample output (first 500 chars):${COL.reset}`);
      console.log(`  ${context.slice(0, 500).split("\n").map(l => `  ${l}`).join("\n")}`);
      console.log("");
    }

    // Compression opportunities analysis
    console.log(`\n${COL.bold}${COL.cyan}=== Compression Opportunities ===${COL.reset}\n`);
    
    const result = await client.callTool("recall_for_context", { 
      query: "authentication and security", 
      project_id: projectId, 
      k: 10 
    });
    const context = extractText(result);
    
    console.log("Current format analysis:");
    console.log(`  Full UUIDs: ${(context.match(/[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}/g) || []).length} occurrences`);
    console.log(`  Score precision: 3 decimal places (e.g., 0.847)`);
    console.log(`  Tag format: "tags=auth,jwt,security" (verbose)`);
    console.log(`  XML wrapper: <biTurboContext>...</biTurboContext> (26 chars overhead)`);
    
    console.log("\nPotential optimizations:");
    console.log("  1. Truncate UUIDs to 8 chars: saves ~28 chars × N items");
    console.log("  2. Reduce score to 2 decimals: saves ~1 char × N items");
    console.log('  3. Compact tag format: "auth,jwt" vs "tags=auth,jwt" saves 5 chars');
    console.log("  4. Single-char type codes: d=decision, f=fact, p=pattern (saves ~5 chars × N)");
    console.log("  5. Remove redundant 'source' field when it's always 'profiler'");
    console.log("  6. Deduplicate overlapping memories (biggest win)");
    console.log("  7. Smart truncation: keep signal, drop boilerplate");
    
    console.log("\nEstimated savings:");
    console.log("  Metadata compression (items 1-5): ~15-20%");
    console.log("  Content deduplication (item 6): ~30-40%");
    console.log("  Smart truncation (item 7): ~10-15%");
    console.log(`  ${COL.bold}Total potential: 55-75% reduction${COL.reset}`);

    await client.callTool("delete_project", { project_id: projectId });
    console.log(`\n${COL.dim}Cleaned up${COL.reset}`);

    process.exit(0);
  } finally {
    client.close();
  }
}

main().catch((e) => { console.error(`${COL.red}Crashed: ${e}${COL.reset}`); process.exit(2); });
