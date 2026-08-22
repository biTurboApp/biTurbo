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
      const timer = setTimeout(() => { this.pending.delete(id); rej(new Error(`timeout: ${method}`)); }, 60_000);
      this.pending.set(id, (r) => { clearTimeout(timer); res(r); });
      this.proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
    });
  }

  async initialize() {
    const r = await this.call("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "extreme-test", version: "0.1" } });
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

function countTokens(text: string): number {
  // Rough estimate: 1 token ≈ 4 chars or 0.75 words
  return Math.ceil(text.length / 4);
}

// Generate 200+ diverse memories
function generateDiverseMemories() {
  const memories: Array<{alias: string; content: string; mem_type: string; tags: string[]; importance: number; domain: string}> = [];
  
  // Software engineering (50 memories)
  const domains = [
    { name: "frontend", topics: ["React hooks", "Vue composition API", "CSS Grid", "TypeScript generics", "Webpack config", "Next.js routing", "Redux middleware", "Testing Library", "Storybook", "Accessibility"] },
    { name: "backend", topics: ["Express middleware", "PostgreSQL queries", "Redis caching", "GraphQL resolvers", "REST API design", "Authentication JWT", "Rate limiting", "Error handling", "Logging", "Database migrations"] },
    { name: "devops", topics: ["Docker containers", "Kubernetes pods", "CI/CD pipelines", "Monitoring", "Load balancing", "SSL certificates", "Backup strategies", "Deployment automation", "Infrastructure as code", "Security scanning"] },
    { name: "mobile", topics: ["React Native", "Swift UI", "Kotlin coroutines", "Push notifications", "App Store submission", "Offline sync", "Biometric auth", "Camera API", "Geolocation", "Battery optimization"] },
    { name: "ml", topics: ["PyTorch models", "TensorFlow graphs", "Feature engineering", "Model deployment", "Data preprocessing", "Hyperparameter tuning", "Transfer learning", "Model evaluation", "A/B testing", "Production monitoring"] },
  ];

  domains.forEach(domain => {
    domain.topics.forEach((topic, i) => {
      memories.push({
        alias: `${domain.name}_${i}`,
        content: `${topic}: Implemented ${topic.toLowerCase()} solution for ${domain.name} project. Used best practices and documented thoroughly. Key decision was to prioritize maintainability over performance.`,
        mem_type: "decision",
        tags: [domain.name, topic.toLowerCase().split(" ")[0], "engineering"],
        importance: 0.6 + Math.random() * 0.3,
        domain: domain.name,
      });
    });
  });

  // Personal preferences (20 memories)
  for (let i = 0; i < 20; i++) {
    memories.push({
      alias: `pref_${i}`,
      content: `User preference #${i}: Prefers ${["concise", "detailed", "visual", "structured", "example-driven"][i % 5]} explanations. Values ${["speed", "accuracy", "creativity", "simplicity"][i % 4]} in responses.`,
      mem_type: "preference",
      tags: ["style", "communication", "preference"],
      importance: 0.7 + Math.random() * 0.25,
      domain: "personal",
    });
  }

  // Business decisions (30 memories)
  for (let i = 0; i < 30; i++) {
    const topics = ["pricing strategy", "marketing campaign", "hiring plan", "product roadmap", "customer feedback", "revenue model", "partnership", "expansion", "cost reduction", "quality assurance"];
    memories.push({
      alias: `biz_${i}`,
      content: `Business decision on ${topics[i % topics.length]}: Chose approach ${String.fromCharCode(65 + (i % 3))} after evaluating trade-offs. Expected impact: ${["high", "medium", "low"][i % 3]}. Timeline: ${i + 1} weeks.`,
      mem_type: "decision",
      tags: ["business", topics[i % topics.length].split(" ")[0], "strategy"],
      importance: 0.65 + Math.random() * 0.3,
      domain: "business",
    });
  }

  // Technical patterns (40 memories)
  for (let i = 0; i < 40; i++) {
    const patterns = ["Singleton", "Factory", "Observer", "Strategy", "Decorator", "Adapter", "Facade", "Proxy", "Command", "Iterator"];
    memories.push({
      alias: `pattern_${i}`,
      content: `Design pattern: ${patterns[i % patterns.length]} pattern applied in ${["API layer", "data access", "UI components", "business logic", "infrastructure"][i % 5]}. Benefits: ${["decoupling", "reusability", "testability", "flexibility"][i % 4]}. Example code included.`,
      mem_type: "pattern",
      tags: ["architecture", patterns[i % patterns.length].toLowerCase(), "design"],
      importance: 0.7 + Math.random() * 0.25,
      domain: "architecture",
    });
  }

  // Episodes/incidents (30 memories)
  for (let i = 0; i < 30; i++) {
    const incidents = ["outage", "bug", "performance issue", "security incident", "data loss", "deployment failure", "integration problem", "user complaint"];
    memories.push({
      alias: `episode_${i}`,
      content: `Incident ${2024}-${String(i + 1).padStart(2, "0")}: ${incidents[i % incidents.length]} in production. Duration: ${i + 1} hours. Root cause: ${["configuration error", "race condition", "resource exhaustion", "third-party failure"][i % 4]}. Resolution: ${["hotfix", "rollback", "config change", "scaling"][i % 4]}.`,
      mem_type: "episode",
      tags: ["incident", incidents[i % incidents.length].split(" ")[0], "production"],
      importance: 0.75 + Math.random() * 0.2,
      domain: "operations",
    });
  }

  // Reflections (30 memories)
  for (let i = 0; i < 30; i++) {
    memories.push({
      alias: `reflection_${i}`,
      content: `Lesson learned #${i + 1}: ${["Technical debt accumulates faster than expected", "Early testing prevents costly rework", "Documentation is as important as code", "User feedback should drive priorities", "Simplicity beats cleverness", "Communication prevents misunderstandings"][i % 6]}. Applied this in ${["project alpha", "project beta", "project gamma"][i % 3]}.`,
      mem_type: "reflection",
      tags: ["lessons", "retrospective", "improvement"],
      importance: 0.7 + Math.random() * 0.25,
      domain: "learning",
    });
  }

  return memories;
}

async function main() {
  const bin = process.argv[2] ?? resolve(process.cwd(), "src-tauri/target/debug/biturbo-mcp.exe");
  if (!existsSync(bin)) { console.error(`${COL.red}Binary not found: ${bin}${COL.reset}`); process.exit(1); }

  console.log(`${COL.bold}${COL.cyan}=== EXTREME Search & Recall Quality Test ===${COL.reset}\n`);

  const client = new McpClient(bin);
  const projectId = `extreme-test-${Date.now().toString(36)}`;

  try {
    await client.initialize();
    await client.callTool("create_project", { id: projectId, name: "Extreme Test", description: "200+ memory stress test" });
    console.log(`${COL.dim}Created project: ${projectId}${COL.reset}\n`);

    // Seed 200+ memories
    const memories = generateDiverseMemories();
    console.log(`${COL.dim}Seeding ${memories.length} diverse memories...${COL.reset}`);
    const t0 = Date.now();
    for (const mem of memories) {
      await client.callTool("remember", {
        content: mem.content,
        mem_type: mem.mem_type,
        project_id: projectId,
        tags: mem.tags,
        importance: mem.importance,
        source_agent: "extreme-test",
      });
    }
    const seedTime = Date.now() - t0;
    console.log(`${COL.green}✓ Seeded ${memories.length} memories in ${(seedTime / 1000).toFixed(1)}s${COL.reset}\n`);

    let totalTests = 0;
    let passedTests = 0;
    const failures: string[] = [];
    const latencies: { search: number[]; recall: number[] } = { search: [], recall: [] };

    // ========== SCALE TESTS ==========
    console.log(`${COL.bold}${COL.cyan}=== 1. SCALE TESTS (200+ memories) ===${COL.reset}\n`);

    const scaleTests = [
      { name: "frontend React", query: "React hooks implementation", expectedDomain: "frontend", k: 10, minHits: 3 },
      { name: "backend database", query: "PostgreSQL database queries", expectedDomain: "backend", k: 10, minHits: 2 },
      { name: "devops deployment", query: "Docker container deployment", expectedDomain: "devops", k: 10, minHits: 3 },
      { name: "mobile app", query: "React Native mobile development", expectedDomain: "mobile", k: 10, minHits: 3 },
      { name: "machine learning", query: "PyTorch machine learning models", expectedDomain: "ml", k: 10, minHits: 3 },
      { name: "business strategy", query: "business pricing strategy", expectedDomain: "business", k: 10, minHits: 3 },
      { name: "design patterns", query: "design pattern architecture", expectedDomain: "architecture", k: 10, minHits: 5 },
      { name: "production incidents", query: "production outage incident", expectedDomain: "operations", k: 10, minHits: 5 },
    ];

    for (const test of scaleTests) {
      console.log(`${COL.bold}${test.name}${COL.reset}`);
      console.log(`  Query: "${test.query}"`);
      
      const t1 = Date.now();
      const searchResult = await client.callTool("search", { query: test.query, project_id: projectId, k: test.k });
      const searchTime = Date.now() - t1;
      latencies.search.push(searchTime);
      
      const hits = extractJson<any[]>(searchResult);
      const domainHits = hits.filter(h => {
        const mem = memories.find(m => m.content.includes(h.content.slice(0, 30)));
        return mem?.domain === test.expectedDomain;
      }).length;
      
      const pass = domainHits >= test.minHits;
      totalTests++;
      if (pass) {
        console.log(`  ${COL.green}✓ PASS${COL.reset} - ${domainHits}/${test.k} hits from ${test.expectedDomain} domain (${searchTime}ms)`);
        passedTests++;
      } else {
        console.log(`  ${COL.red}✗ FAIL${COL.reset} - only ${domainHits}/${test.k} hits from ${test.expectedDomain} (min: ${test.minHits}) (${searchTime}ms)`);
        failures.push(`${test.name}: expected ${test.expectedDomain} domain, got ${domainHits}/${test.k} (min: ${test.minHits})`);
      }
    }

    // ========== EDGE CASE TESTS ==========
    console.log(`\n${COL.bold}${COL.cyan}=== 2. EDGE CASE TESTS ===${COL.reset}\n`);

    const edgeTests = [
      { name: "empty query", query: "", shouldReturn: 0 },
      { name: "single char", query: "a", shouldReturn: "any" },
      { name: "single word", query: "React", shouldReturn: "any" },
      { name: "very long query", query: "What is the best approach to implement a scalable microservices architecture with proper service discovery, load balancing, circuit breakers, and distributed tracing while maintaining high availability and fault tolerance across multiple cloud regions?".repeat(2), shouldReturn: "any" },
      { name: "unicode", query: "React ⚛️ hooks 🪝 TypeScript", shouldReturn: "any" },
      { name: "special chars", query: "SELECT * FROM users WHERE id = 1; DROP TABLE users;--", shouldReturn: "any" },
      { name: "only stopwords", query: "the and or but in on at to for of with", shouldReturn: "any" },
      { name: "nonsense", query: "xyz123 abc456 qwe789", shouldReturn: "any" },
      { name: "SQL injection", query: "'; DROP TABLE memories; --", shouldReturn: "any" },
    ];

    for (const test of edgeTests) {
      console.log(`${COL.bold}${test.name}${COL.reset}`);
      console.log(`  Query: "${test.query.slice(0, 60)}${test.query.length > 60 ? "..." : ""}"`);
      
      try {
        const t1 = Date.now();
        const searchResult = await client.callTool("search", { query: test.query, project_id: projectId, k: 5 });
        const searchTime = Date.now() - t1;
        latencies.search.push(searchTime);
        
        const hits = extractJson<any[]>(searchResult);
        totalTests++;
        
        if (test.shouldReturn === 0) {
          if (hits.length === 0) {
            console.log(`  ${COL.green}✓ PASS${COL.reset} - correctly returned 0 results (${searchTime}ms)`);
            passedTests++;
          } else {
            console.log(`  ${COL.red}✗ FAIL${COL.reset} - expected 0 results, got ${hits.length} (${searchTime}ms)`);
            failures.push(`${test.name}: expected 0 results for empty query`);
          }
        } else {
          console.log(`  ${COL.green}✓ PASS${COL.reset} - returned ${hits.length} results without crashing (${searchTime}ms)`);
          passedTests++;
        }
      } catch (e: any) {
        totalTests++;
        console.log(`  ${COL.red}✗ FAIL${COL.reset} - crashed: ${e.message}`);
        failures.push(`${test.name}: ${e.message}`);
      }
    }

    // ========== ADVERSARIAL TESTS ==========
    console.log(`\n${COL.bold}${COL.cyan}=== 3. ADVERSARIAL TESTS ===${COL.reset}\n`);

    const adversarialTests = [
      { name: "ambiguous term", query: "deployment", shouldNotBeAllFrom: ["frontend"] },
      { name: "high-importance trap", query: "random unrelated topic xyz", shouldNotHaveMinImportance: 0.9 },
    ];

    for (const test of adversarialTests) {
      console.log(`${COL.bold}${test.name}${COL.reset}`);
      console.log(`  Query: "${test.query}"`);
      
      const t1 = Date.now();
      const searchResult = await client.callTool("search", { query: test.query, project_id: projectId, k: 10 });
      const searchTime = Date.now() - t1;
      latencies.search.push(searchTime);
      
      const hits = extractJson<any[]>(searchResult);
      totalTests++;
      
      if (test.shouldNotBeAllFrom) {
        const domains = hits.map(h => {
          const mem = memories.find(m => m.content.includes(h.content.slice(0, 30)));
          return mem?.domain;
        });
        const allFromForbidden = domains.every(d => test.shouldNotBeAllFrom!.includes(d!));
        if (!allFromForbidden) {
          console.log(`  ${COL.green}✓ PASS${COL.reset} - results diversified across domains (${searchTime}ms)`);
          passedTests++;
        } else {
          console.log(`  ${COL.red}✗ FAIL${COL.reset} - all results from ${test.shouldNotBeAllFrom.join(", ")} (${searchTime}ms)`);
          failures.push(`${test.name}: results not diversified`);
        }
      } else if (test.shouldNotHaveMinImportance !== undefined) {
        const lowImportanceHits = hits.filter(h => h.importance < test.shouldNotHaveMinImportance!);
        if (lowImportanceHits.length > 0) {
          console.log(`  ${COL.green}✓ PASS${COL.reset} - correctly returned low-importance results (${searchTime}ms)`);
          passedTests++;
        } else {
          console.log(`  ${COL.red}✗ FAIL${COL.reset} - only returned high-importance results (${searchTime}ms)`);
          failures.push(`${test.name}: importance filtering broken`);
        }
      }
    }

    // ========== CONTEXT BUDGET TEST ==========
    console.log(`\n${COL.bold}${COL.cyan}=== 4. CONTEXT BUDGET TEST ===${COL.reset}\n`);

    const contextTest = { name: "large context", query: "React frontend", k: 20 };
    console.log(`${COL.bold}${contextTest.name}${COL.reset}`);
    console.log(`  Query: "${contextTest.query}" (k=${contextTest.k})`);
    
    const t1 = Date.now();
    const recallResult = await client.callTool("recall_for_context", { query: contextTest.query, project_id: projectId, k: contextTest.k });
    const recallTime = Date.now() - t1;
    latencies.recall.push(recallTime);
    
    const context = extractText(recallResult);
    const charCount = context.length;
    const tokenEstimate = countTokens(context);
    const maxChars = 12000;
    
    totalTests++;
    if (charCount <= maxChars) {
      console.log(`  ${COL.green}✓ PASS${COL.reset} - context budget: ${charCount} chars / ${tokenEstimate} tokens (limit: ${maxChars}) (${recallTime}ms)`);
      passedTests++;
    } else {
      console.log(`  ${COL.red}✗ FAIL${COL.reset} - context exceeded: ${charCount} chars > ${maxChars} limit (${recallTime}ms)`);
      failures.push(`context budget: ${charCount} chars exceeded ${maxChars} limit`);
    }

    // ========== LATENCY BENCHMARK ==========
    console.log(`\n${COL.bold}${COL.cyan}=== 5. LATENCY BENCHMARK ===${COL.reset}\n`);

    const benchmarkQueries = [
      "React hooks",
      "database optimization",
      "deployment strategy",
      "machine learning models",
      "business decisions",
    ];

    for (const query of benchmarkQueries) {
      const searchTimes: number[] = [];
      const recallTimes: number[] = [];
      
      for (let i = 0; i < 5; i++) {
        const t1 = Date.now();
        await client.callTool("search", { query, project_id: projectId, k: 10 });
        searchTimes.push(Date.now() - t1);
        
        const t2 = Date.now();
        await client.callTool("recall_for_context", { query, project_id: projectId, k: 10 });
        recallTimes.push(Date.now() - t2);
      }
      
      const avgSearch = searchTimes.reduce((a, b) => a + b) / searchTimes.length;
      const avgRecall = recallTimes.reduce((a, b) => a + b) / recallTimes.length;
      
      console.log(`  "${query}"`);
      console.log(`    search:  avg ${avgSearch.toFixed(0)}ms, p95 ${Math.max(...searchTimes)}ms`);
      console.log(`    recall:  avg ${avgRecall.toFixed(0)}ms, p95 ${Math.max(...recallTimes)}ms`);
    }

    // ========== SUMMARY ==========
    console.log(`\n${COL.bold}${COL.cyan}=== SUMMARY ===${COL.reset}\n`);
    console.log(`  Total tests: ${totalTests}`);
    console.log(`  ${COL.green}Passed: ${passedTests}${COL.reset}`);
    console.log(`  ${COL.red}Failed: ${totalTests - passedTests}${COL.reset}`);
    console.log(`  Pass rate: ${((passedTests / totalTests) * 100).toFixed(1)}%\n`);

    if (latencies.search.length > 0) {
      const sortedSearch = [...latencies.search].sort((a, b) => a - b);
      console.log(`  Search latency:`);
      console.log(`    p50: ${sortedSearch[Math.floor(sortedSearch.length * 0.5)]}ms`);
      console.log(`    p95: ${sortedSearch[Math.floor(sortedSearch.length * 0.95)]}ms`);
      console.log(`    p99: ${sortedSearch[Math.floor(sortedSearch.length * 0.99)]}ms`);
    }

    if (latencies.recall.length > 0) {
      const sortedRecall = [...latencies.recall].sort((a, b) => a - b);
      console.log(`  Recall latency:`);
      console.log(`    p50: ${sortedRecall[Math.floor(sortedRecall.length * 0.5)]}ms`);
      console.log(`    p95: ${sortedRecall[Math.floor(sortedRecall.length * 0.95)]}ms`);
      console.log(`    p99: ${sortedRecall[Math.floor(sortedRecall.length * 0.99)]}ms`);
    }

    if (failures.length > 0) {
      console.log(`\n${COL.red}Failures:${COL.reset}`);
      failures.forEach(f => console.log(`  - ${f}`));
    }

    await client.callTool("delete_project", { project_id: projectId });
    console.log(`\n${COL.dim}Cleaned up test project${COL.reset}`);

    process.exit(failures.length > 0 ? 1 : 0);
  } finally {
    client.close();
  }
}

main().catch((e) => { console.error(`${COL.red}Crashed: ${e}${COL.reset}`); process.exit(2); });
