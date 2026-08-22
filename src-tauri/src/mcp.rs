use crate::error::{BiError, BiResult};
use crate::ingest;
use crate::memory::{self, Memory, MemoryWithScore, RememberInput, UpdateInput};
use crate::project::{self, CreateProjectInput};
use crate::state::AppState;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run_mcp_server_stdio() -> anyhow::Result<()> {
    let data_dir = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no data dir"))?
        .join("com.biturbo.app");
    std::fs::create_dir_all(&data_dir).ok();
    let state = Arc::new(AppState::open(&data_dir)?);
    crate::operations::resume_pending(state.clone())?;

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => dispatch(&state, req).await,
            Err(e) => json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            }),
        };
        let out = serde_json::to_string(&response).unwrap_or_default();
        stdout.write_all(out.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn dispatch(state: &Arc<AppState>, req: JsonRpcRequest) -> Value {
    let id = req.id.clone().unwrap_or(Value::Null);
    match req.method.as_str() {
        "initialize" => {
            let mut response = ok(
                &id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "biTurbo", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "## biTurbo Memory Layer — Instructions\n\nYou have access to biTurbo, a persistent semantic memory layer via MCP.\n\n## Core loop:\n1. **RECALL** — call `recall_for_context(query=<user msg>, project_id=<current>, k=8)`.\n2. **ANSWER** — respond using recalled context.\n3. **REMEMBER** — store only durable, useful information.\n\n## When to `remember`:\n- ✅ User states a fact about themselves/environment/project → `fact`\n- ✅ You make a decision with rationale → `decision`\n- ✅ User expresses a preference (style, verbosity, tools) → `preference`\n- ✅ User corrects you → `fact` with `supersedes`\n- ✅ You discover a codebase pattern → `pattern`\n- ✅ Something noteworthy happened → `episode`\n- ✅ Meta-observation about user or work → `reflection`\n- ❌ Transient state — don't remember\n- ❌ Public knowledge any LLM knows — don't remember\n- ❌ Secrets, tokens, PII — **NEVER**\n\nIf unsure: \"Would future-me in 6 months want to know this?\" If yes, remember.\n\n## Memory types:\n- `fact` — verifiable facts\n- `decision` — choices + why\n- `preference` — how user wants things\n- `pattern` — repeatable approaches\n- `episode` — past events (include timestamp)\n- `reflection` — meta-observations\n- `code` — set by ingest_project only\n\n## Importance (0-1):\n- 0.8-1.0: cross-project rules, key decisions\n- 0.5-0.7: typical (default 0.6)\n- 0.2-0.4: specific/stale details\n\n## Tags: 1-3 per memory. Good: `auth`, `ui`, `db`, `convention`, `api`. Bad: `important`, `todo`.\n\n## Session lifecycle:\n- START → `register_agent(name, kind)`, `list_projects()`\n- EVERY TURN → recall before non-trivial work\n- END → `consolidate(project_id)`, final `remember`\n\n## Multi-project:\n- Always pass `project_id`. Isolated per project.\n- `project_id=\"default\"` for cross-cutting facts.\n\n## Anti-patterns:\n- Don't dump 10k memories — use recall_for_context k=5-10\n- Don't skip recall for project-specific work — amnesia is worse than no tool\n- Don't remember the obvious (Cargo, Git, syntax)\n- Don't remember every response — memory quality matters more than volume\n- Don't forget prematurely — knowledge dies\n- Never cross-project leak — right project_id always\n- Never store secrets, tokens, PII\n\n## Tools (20):\nremember, forget, update, get_memory, search, list, list_tags,\nrecall_for_context, list_projects, get_project, create_project,\ndelete_project, ingest_project, consolidate, consolidate_status,\nget_project_name_from_file,\nstats, bootstrap, recent_activity, register_agent"
                }),
            );
            response["result"]["instructions"] = Value::String(MCP_INSTRUCTIONS.into());
            response
        }
        "notifications/initialized" => json!({}),
        "tools/list" => ok(&id, json!({ "tools": tool_schemas() })),
        "tools/call" => {
            let params = req.params;
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(state, name, args).await {
                Ok(content) => ok(&id, json!({ "content": content, "isError": false })),
                Err(e) => ok(
                    &id,
                    json!({
                        "content": [{ "type": "text", "text": format!("error: {e}") }],
                        "isError": true
                    }),
                ),
            }
        }
        "ping" => ok(&id, json!({})),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {}", req.method) }
        }),
    }
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

const MCP_INSTRUCTIONS: &str = r#"## biTurbo Memory Layer

Use `register_agent` and `list_projects` at session start. Before non-trivial work, call `recall_for_context` with the active project. Store only durable facts, decisions, preferences, patterns, episodes, reflections, or indexed code; never store secrets or transient state. Always pass `project_id` to preserve isolation.

The 27-tool public surface includes compatible memory/project APIs plus explainable recall (`recall_explain`, `submit_recall_feedback`) and supervised operations (`start_ingest`, `operation_status`, `list_operations`, `cancel_operation`, `retry_operation`). Legacy `ingest_project` and `consolidate` remain synchronous."#;

async fn call_tool(state: &Arc<AppState>, name: &str, args: Value) -> BiResult<Vec<Value>> {
    let text = |v: &str| vec![json!({ "type": "text", "text": v })];
    let require_project = |pid: &str| -> BiResult<()> {
        project::get(state, pid).map(|_| ()).map_err(|_| {
            crate::error::BiError::Invalid(format!(
                "project '{pid}' does not exist — create it first with create_project"
            ))
        })
    };
    let require_path = |path: &str, label: &str| -> BiResult<()> {
        if !std::path::Path::new(path).exists() {
            return Err(crate::error::BiError::Invalid(format!(
                "{label} '{path}' does not exist on disk"
            )));
        }
        Ok(())
    };
    let result = match name {
        "remember" => {
            let input: RememberInput = serde_json::from_value(args.clone())?;
            if let Some(pid) = input.project_id.as_deref() {
                require_project(pid)?;
            }
            let m = memory::remember(state, input)?;
            text(&serde_json::to_string_pretty(&m)?)
        }
        "forget" => {
            let uid = arg_str(&args, "uid")?;
            let b = memory::forget(state, &uid)?;
            text(&serde_json::to_string_pretty(&json!({ "forgotten": b }))?)
        }
        "update" => {
            let uid = arg_str(&args, "uid")?;
            let input: UpdateInput = serde_json::from_value(args)?;
            let m = memory::update(state, &uid, input)?;
            text(&serde_json::to_string_pretty(&m)?)
        }
        "get_memory" => {
            let uid = arg_str(&args, "uid")?;
            let m = memory::get(state, &uid)?;
            text(&serde_json::to_string_pretty(&m)?)
        }
        "search" => {
            let project_id = resolve_project_from_args(state, &args)?;
            let query = arg_str(&args, "query")?;
            let k = bounded_k(&args, 10, 100);
            let mem_type = args.get("mem_type").and_then(|v| v.as_str());
            let hits: Vec<MemoryWithScore> =
                memory::search(state, &project_id, &query, k, mem_type)?;
            text(&serde_json::to_string_pretty(&hits)?)
        }
        "list" => {
            let project_id = args.get("project_id").and_then(|v| v.as_str());
            let mem_type = args.get("mem_type").and_then(|v| v.as_str());
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let m: Vec<Memory> = memory::list(state, project_id, mem_type, limit, offset)?;
            text(&serde_json::to_string_pretty(&m)?)
        }
        "list_tags" => {
            let project_id = args.get("project_id").and_then(|v| v.as_str());
            let t = memory::list_tags(state, project_id)?;
            text(&serde_json::to_string_pretty(&t)?)
        }
        "recall_for_context" => {
            let project_id = resolve_project_from_args(state, &args)?;
            let query = arg_str(&args, "query")?;
            let k = bounded_k(&args, 8, 20);
            let mem_type = args.get("mem_type").and_then(|v| v.as_str());
            let hits = memory::search(state, &project_id, &query, k, mem_type)?;
            text(&format_context_block(&hits))
        }
        "recall_explain" => {
            let project_id = resolve_project_from_args(state, &args)?;
            let query = arg_str(&args, "query")?;
            let k = bounded_k(&args, 8, 20);
            let mem_type = args.get("mem_type").and_then(|v| v.as_str());
            let response = crate::recall::explain(state, &project_id, &query, k, mem_type)?;
            text(&serde_json::to_string_pretty(&response)?)
        }
        "submit_recall_feedback" => {
            let recall_id = arg_str(&args, "recall_id")?;
            let memory_uid = arg_str(&args, "memory_uid")?;
            let value = args.get("value").and_then(|v| v.as_i64()).unwrap_or(1) as i8;
            let source = args
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("explicit");
            crate::recall::submit_feedback(state, &recall_id, &memory_uid, value, source)?;
            text("{\"recorded\":true}")
        }
        "list_projects" => text(&serde_json::to_string_pretty(&project::list(state)?)?),
        "get_project" => {
            let id = arg_str(&args, "id")?;
            let p = project::get(state, &id)?;
            text(&serde_json::to_string_pretty(&p)?)
        }
        "create_project" => {
            let name = arg_str(&args, "name")?;
            let input = CreateProjectInput {
                id: args.get("id").and_then(|v| v.as_str().map(String::from)),
                name,
                description: args
                    .get("description")
                    .and_then(|v| v.as_str().map(String::from)),
                root_path: args
                    .get("root_path")
                    .and_then(|v| v.as_str().map(String::from)),
                bit_width: args
                    .get("bit_width")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u8),
            };
            let p = project::create(state, input)?;
            text(&serde_json::to_string_pretty(&p)?)
        }
        "delete_project" => {
            let id = arg_str(&args, "project_id")?;
            require_project(&id)?;
            project::delete(state, &id)?;
            text(&serde_json::to_string_pretty(&json!({ "deleted": id }))?)
        }
        "ingest_project" => {
            let project_id = arg_str(&args, "project_id")?;
            let root_path = arg_str(&args, "root_path")?;
            require_project(&project_id)?;
            require_path(&root_path, "root_path")?;
            let r = crate::operations::run_ingest_blocking(
                state,
                &project_id,
                std::path::Path::new(&root_path),
            )?;
            text(&serde_json::to_string_pretty(&r)?)
        }
        "start_ingest" => {
            let project_id = arg_str(&args, "project_id")?;
            let root_path = arg_str(&args, "root_path")?;
            require_project(&project_id)?;
            require_path(&root_path, "root_path")?;
            let operation = crate::operations::start_ingest(
                state,
                &project_id,
                std::path::Path::new(&root_path),
            )?;
            text(&serde_json::to_string_pretty(&operation)?)
        }
        "operation_status" => {
            let id = arg_str(&args, "id")?;
            text(&serde_json::to_string_pretty(&crate::operations::get(
                state, &id,
            )?)?)
        }
        "list_operations" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            text(&serde_json::to_string_pretty(&crate::operations::list(
                state, limit,
            )?)?)
        }
        "cancel_operation" => {
            let id = arg_str(&args, "id")?;
            text(&serde_json::to_string_pretty(
                &crate::operations::request_cancel(state, &id)?,
            )?)
        }
        "retry_operation" => {
            let id = arg_str(&args, "id")?;
            text(&serde_json::to_string_pretty(&crate::operations::retry(
                state, &id,
            )?)?)
        }
        "get_project_graph" => {
            let project_id = arg_str(&args, "project_id")?;
            let g = ingest::get_project_graph(state, &project_id)?;
            text(&serde_json::to_string_pretty(&g)?)
        }
        "consolidate" => {
            let project_id = args.get("project_id").and_then(|v| v.as_str());
            if let Some(p) = project_id {
                require_project(p)?;
            }
            let r = crate::operations::run_consolidate_blocking(state, project_id)?;
            text(&serde_json::to_string_pretty(&r)?)
        }
        "consolidate_status" => {
            let s = crate::scheduler::get_status();
            text(&serde_json::to_string_pretty(&s)?)
        }
        "import_folder" => {
            let project_id = arg_str(&args, "project_id")?;
            let root_path = arg_str(&args, "root_path")?;
            require_project(&project_id)?;
            require_path(&root_path, "root_path")?;
            let r = crate::io::import_folder(state, &project_id, std::path::Path::new(&root_path))?;
            text(&serde_json::to_string_pretty(&r)?)
        }
        "export_memories" => {
            let project_id = args.get("project_id").and_then(|v| v.as_str());
            if let Some(p) = project_id {
                require_project(p)?;
            }
            let output_path = arg_str(&args, "output_path")?;
            let r =
                crate::io::export_memories(state, project_id, std::path::Path::new(&output_path))?;
            text(&serde_json::to_string_pretty(&r)?)
        }
        "enable_watch" => {
            let project_id = arg_str(&args, "project_id")?;
            let root_path = arg_str(&args, "root_path")?;
            require_project(&project_id)?;
            require_path(&root_path, "root_path")?;
            crate::io::enable_watch(state, &project_id, std::path::Path::new(&root_path))?;
            let s = crate::io::watch_status();
            text(&serde_json::to_string_pretty(&s)?)
        }
        "disable_watch" => {
            let project_id = arg_str(&args, "project_id")?;
            require_project(&project_id)?;
            crate::io::disable_watch(state, &project_id)?;
            let s = crate::io::watch_status();
            text(&serde_json::to_string_pretty(&s)?)
        }
        "watch_status" => {
            let s = crate::io::watch_status();
            text(&serde_json::to_string_pretty(&s)?)
        }
        "set_project_embed_model" => {
            let project_id = arg_str(&args, "project_id")?;
            require_project(&project_id)?;
            let model = args.get("model").and_then(|v| v.as_str()).map(String::from);
            crate::io::set_project_embed_model(state, &project_id, model.as_deref())?;
            text("{}")
        }
        "stats" => text(&serde_json::to_string_pretty(&crate::application::stats(
            state,
        )?)?),
        "bootstrap" => text(&serde_json::to_string_pretty(
            &crate::application::bootstrap(state)?,
        )?),
        "recent_activity" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            text(&serde_json::to_string_pretty(
                &crate::application::recent_activity(state, limit)?,
            )?)
        }
        "register_agent" => {
            let name = arg_str(&args, "name")?;
            let kind = arg_str(&args, "kind")?;
            let meta = args.get("meta").cloned();
            text(&serde_json::to_string_pretty(
                &crate::application::register_agent(state, name, kind, meta)?,
            )?)
        }
        "get_project_name_from_file" => {
            let root_path = arg_str(&args, "root_path")?;
            require_path(&root_path, "root_path")?;
            let biturbo_file = std::path::PathBuf::from(root_path).join(".biTurbo");
            match std::fs::read_to_string(&biturbo_file) {
                Ok(content) => {
                    let project_name = content
                        .lines()
                        .find(|line| line.starts_with("projectName="))
                        .and_then(|line| line.strip_prefix("projectName="))
                        .map(String::from);
                    match project_name {
                        Some(name) => text(&serde_json::to_string_pretty(
                            &json!({ "projectName": name }),
                        )?),
                        None => text(&serde_json::to_string_pretty(
                            &json!({ "error": "projectName not set in .biTurbo file" }),
                        )?),
                    }
                }
                Err(_) => text(&serde_json::to_string_pretty(
                    &json!({ "error": ".biTurbo file not found" }),
                )?),
            }
        }
        other => return Err(BiError::Invalid(format!("unknown tool: {other}"))),
    };
    Ok(result)
}

fn arg_str(args: &Value, key: &str) -> BiResult<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| BiError::Invalid(format!("missing string arg: {key}")))
}

fn bounded_k(args: &Value, default: u64, max: u64) -> usize {
    args.get("k")
        .and_then(|v| v.as_u64())
        .unwrap_or(default)
        .clamp(1, max) as usize
}

fn resolve_project_from_args(state: &AppState, args: &Value) -> BiResult<String> {
    let project_id = args.get("project_id").and_then(|v| v.as_str());
    let root_path = args.get("root_path").and_then(|v| v.as_str());
    project::resolve_project_id(state, project_id, root_path)
}

const RECALL_CONTEXT_MAX_CHARS: usize = 12_000;
const RECALL_ITEM_MAX_CHARS: usize = 1_200;

/// Map memory type string to single-char code for compact output.
fn type_code(mem_type: &str) -> char {
    match mem_type {
        "fact" => 'f',
        "decision" => 'd',
        "preference" => 'p',
        "pattern" => 'P',
        "episode" => 'e',
        "reflection" => 'r',
        "code" => 'c',
        _ => '?',
    }
}

/// Smart truncation at sentence boundary. Falls back to hard cut if no boundary found.
fn trim_for_context(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    // Try to cut at a sentence boundary (`.`, `!`, `?`, `\n`) near max_chars
    let chars: Vec<char> = trimmed.chars().collect();
    let mut cut = max_chars.min(chars.len());

    // Walk backwards from max_chars looking for sentence end
    let search_start = max_chars.saturating_sub(max_chars / 3);
    for i in (search_start..cut).rev() {
        if i < chars.len() && matches!(chars[i], '.' | '!' | '?' | '\n') {
            cut = i + 1;
            break;
        }
    }

    let mut out: String = chars[..cut.min(chars.len())].iter().collect();
    out.push('…');
    out
}

/// Compute word-level Jaccard similarity between two texts (cheap proxy for semantic overlap).
fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// Filter near-duplicate hits: if two memories have Jaccard similarity > threshold,
/// keep only the higher-scored one. Prevents wasting context budget on redundant info.
fn deduplicate_hits(hits: &[MemoryWithScore], threshold: f32) -> Vec<MemoryWithScore> {
    let mut kept: Vec<MemoryWithScore> = Vec::with_capacity(hits.len());
    let mut skip: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for i in 0..hits.len() {
        if skip.contains(&i) {
            continue;
        }
        for j in (i + 1)..hits.len() {
            if skip.contains(&j) {
                continue;
            }
            let sim = jaccard_similarity(&hits[i].memory.content, &hits[j].memory.content);
            if sim >= threshold {
                // Keep the one with higher score (already sorted descending)
                skip.insert(j);
            }
        }
        kept.push(hits[i].clone());
    }
    kept
}

fn format_context_block(hits: &[MemoryWithScore]) -> String {
    if hits.is_empty() {
        return "<biTurboContext>no relevant memories</biTurboContext>".into();
    }

    // Deduplicate near-identical memories before formatting
    let deduped = deduplicate_hits(hits, 0.55);

    let mut s = String::from("<ctx>\n");
    for (i, h) in deduped.iter().enumerate() {
        let tc = type_code(&h.memory.mem_type);
        let tags = h.memory.tags.join(",");

        // Compact single-line header: [N] type|score|importance|tags
        s.push_str(&format!(
            "[{}] {}|{:.2}|{:.2}|{}\n",
            i + 1,
            tc,
            h.score,
            h.memory.importance,
            tags,
        ));

        // Optional location line for code memories
        if let Some(path) = h.memory.file_path.as_deref() {
            let range = match (h.memory.start_line, h.memory.end_line) {
                (Some(start), Some(end)) => format!(":{start}-{end}"),
                _ => String::new(),
            };
            let lang = h.memory.language.as_deref().unwrap_or("");
            s.push_str(&format!("> {}{} {}\n", path, range, lang));
        }

        s.push_str(trim_for_context(&h.memory.content, RECALL_ITEM_MAX_CHARS).as_str());
        s.push('\n');

        if s.chars().count() >= RECALL_CONTEXT_MAX_CHARS {
            break;
        }
    }
    s.push_str("</ctx>");
    s
}

fn tool_schemas() -> Value {
    serde_json::from_str(SCHEMAS_JSON).unwrap_or_else(|_| json!([]))
}

const SCHEMAS_JSON: &str = r#"[
{"name":"remember","description":"Store a memory. mem_type: fact|decision|preference|pattern|episode|reflection|code.","inputSchema":{"type":"object","required":["content"],"properties":{"content":{"type":"string"},"mem_type":{"type":"string"},"project_id":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}},"importance":{"type":"number"},"source_agent":{"type":"string"},"supersedes":{"type":"string"}}}},
{"name":"forget","description":"Delete a memory by uid.","inputSchema":{"type":"object","required":["uid"],"properties":{"uid":{"type":"string"}}}},
{"name":"update","description":"Edit a memory. Any omitted field is unchanged.","inputSchema":{"type":"object","required":["uid"],"properties":{"uid":{"type":"string"},"content":{"type":"string"},"mem_type":{"type":"string"},"tags":{"type":"array","items":{"type":"string"}},"importance":{"type":"number"}}}},
{"name":"get_memory","description":"Fetch one memory by uid.","inputSchema":{"type":"object","required":["uid"],"properties":{"uid":{"type":"string"}}}},
{"name":"search","description":"Semantic search. Pass project_id or root_path (reads .biTurbo). mem_type filters. k=top-N (default 10).","inputSchema":{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"project_id":{"type":"string"},"root_path":{"type":"string"},"mem_type":{"type":"string"},"k":{"type":"number"}}}},
{"name":"list","description":"List memories with optional filters. Newest first. Default 50.","inputSchema":{"type":"object","properties":{"project_id":{"type":"string"},"mem_type":{"type":"string"},"limit":{"type":"number"},"offset":{"type":"number"}}}},
{"name":"list_tags","description":"List tags for a project with usage counts. Newest first.","inputSchema":{"type":"object","properties":{"project_id":{"type":"string"}},"required":["project_id"]}},
{"name":"recall_for_context","description":"Build a <biTurboContext> block of top-k relevant memories. Pass project_id or root_path (reads .biTurbo).","inputSchema":{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"project_id":{"type":"string"},"root_path":{"type":"string"},"mem_type":{"type":"string"},"k":{"type":"number"}}}},
{"name":"recall_explain","description":"Recall ranked memories with source ranks, matched terms, feedback boost, and a recall id.","inputSchema":{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"project_id":{"type":"string"},"root_path":{"type":"string"},"mem_type":{"type":"string"},"k":{"type":"number"}}}},
{"name":"submit_recall_feedback","description":"Record useful or not-useful feedback for one recalled memory.","inputSchema":{"type":"object","required":["recall_id","memory_uid","value"],"properties":{"recall_id":{"type":"string"},"memory_uid":{"type":"string"},"value":{"type":"number"},"source":{"type":"string"}}}},
{"name":"list_projects","description":"List all projects.","inputSchema":{"type":"object","properties":{}}},
{"name":"get_project","description":"Fetch one project by id.","inputSchema":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}},
{"name":"create_project","description":"Create a new project.","inputSchema":{"type":"object","required":["name"],"properties":{"name":{"type":"string"},"id":{"type":"string"},"description":{"type":"string"},"root_path":{"type":"string"},"bit_width":{"type":"number"}}}},
{"name":"delete_project","description":"Delete a project and all its memories. 'default' cannot be deleted.","inputSchema":{"type":"object","required":["project_id"],"properties":{"project_id":{"type":"string"}}}},
{"name":"ingest_project","description":"Index a code directory via tree-sitter (22 languages, including rust/typescript/python/go/kotlin/sql/dart/lua/scala/r/powershell).","inputSchema":{"type":"object","required":["project_id","root_path"],"properties":{"project_id":{"type":"string"},"root_path":{"type":"string"}}}},
{"name":"start_ingest","description":"Start an asynchronous supervised ingest and return its operation record.","inputSchema":{"type":"object","required":["project_id","root_path"],"properties":{"project_id":{"type":"string"},"root_path":{"type":"string"}}}},
{"name":"operation_status","description":"Get one persisted operation by id.","inputSchema":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}},
{"name":"list_operations","description":"List recent supervised operations.","inputSchema":{"type":"object","properties":{"limit":{"type":"number"}}}},
{"name":"cancel_operation","description":"Request operation cancellation at its next safe checkpoint.","inputSchema":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}},
{"name":"retry_operation","description":"Retry a failed or cancelled operation from its persisted input checkpoint.","inputSchema":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}}},
{"name":"consolidate","description":"Run memory maintenance: decay, dedup (cosine >= 0.95), merge.","inputSchema":{"type":"object","properties":{"project_id":{"type":"string"}}}},
{"name":"consolidate_status","description":"Status of the background consolidate scheduler (running/idle, last run, next run).","inputSchema":{"type":"object","properties":{}}},
{"name":"stats","description":"Global stats.","inputSchema":{"type":"object","properties":{}}},
{"name":"bootstrap","description":"One-call page mount: stats + projects + recent + tags + agents + consolidate status.","inputSchema":{"type":"object","properties":{}}},
{"name":"recent_activity","description":"Recent activity entries.","inputSchema":{"type":"object","properties":{"limit":{"type":"number"}}}},
{"name":"register_agent","description":"Register or update this agent's record. Call once per session.","inputSchema":{"type":"object","required":["name","kind"],"properties":{"name":{"type":"string"},"kind":{"type":"string"},"meta":{"type":"object"}}}},
{"name":"get_project_name_from_file","description":"Read projectName from .biTurbo file in project root. Returns {\"projectName\": \"...\"} or {\"error\": \"...\"}.","inputSchema":{"type":"object","required":["root_path"],"properties":{"root_path":{"type":"string"}}}}
]"#;
