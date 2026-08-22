//! Tauri IPC commands. The frontend calls these via `invoke<T>("name", { args })`.

pub use crate::application::{ActivityEntry, AgentEntry, Bootstrap, Stats};
use crate::error::BiResult;
use crate::ingest;
use crate::memory::{self, Memory, MemoryWithScore, RememberInput, UpdateInput};
use crate::project::{self, CreateProjectInput, Project};
use crate::scheduler::ConsolidateStatus;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

#[tauri::command]
pub fn list_memories(
    state: State<'_, AppState>,
    project_id: Option<String>,
    mem_type: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> BiResult<Vec<Memory>> {
    memory::list(
        state.inner(),
        project_id.as_deref(),
        mem_type.as_deref(),
        limit.unwrap_or(50),
        offset.unwrap_or(0),
    )
}

#[tauri::command]
pub fn list_tags(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> BiResult<Vec<(String, i64)>> {
    memory::list_tags(state.inner(), project_id.as_deref())
}

#[tauri::command]
pub fn get_memory(state: State<'_, AppState>, uid: String) -> BiResult<Option<Memory>> {
    memory::get(state.inner(), &uid)
}

#[tauri::command]
pub fn remember(state: State<'_, AppState>, input: RememberInput) -> BiResult<Memory> {
    memory::remember(state.inner(), input)
}

#[tauri::command]
pub fn forget_memory(state: State<'_, AppState>, uid: String) -> BiResult<bool> {
    memory::forget(state.inner(), &uid)
}

#[tauri::command]
pub fn update_memory(
    state: State<'_, AppState>,
    uid: String,
    input: UpdateInput,
) -> BiResult<Memory> {
    memory::update(state.inner(), &uid, input)
}

#[derive(Deserialize)]
pub struct SearchArgs {
    pub project_id: Option<String>,
    pub query: String,
    pub k: Option<usize>,
    pub mem_type: Option<String>,
}

#[tauri::command]
pub fn search_memories(
    state: State<'_, AppState>,
    args: SearchArgs,
) -> BiResult<Vec<MemoryWithScore>> {
    memory::search(
        state.inner(),
        args.project_id.as_deref().unwrap_or(""),
        &args.query,
        args.k.unwrap_or(10),
        args.mem_type.as_deref(),
    )
}

#[tauri::command]
pub fn recall_explain(
    state: State<'_, AppState>,
    args: SearchArgs,
) -> BiResult<crate::recall::RecallResponse> {
    crate::recall::explain(
        state.inner(),
        args.project_id.as_deref().unwrap_or(""),
        &args.query,
        args.k.unwrap_or(10),
        args.mem_type.as_deref(),
    )
}

#[derive(Deserialize)]
pub struct RecallFeedbackArgs {
    pub recall_id: String,
    pub memory_uid: String,
    pub value: i8,
    pub source: Option<String>,
}

#[tauri::command]
pub fn submit_recall_feedback(
    state: State<'_, AppState>,
    args: RecallFeedbackArgs,
) -> BiResult<()> {
    crate::recall::submit_feedback(
        state.inner(),
        &args.recall_id,
        &args.memory_uid,
        args.value,
        args.source.as_deref().unwrap_or("explicit"),
    )
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> BiResult<Vec<Project>> {
    project::list(state.inner())
}

#[tauri::command]
pub fn get_project(state: State<'_, AppState>, id: String) -> BiResult<Project> {
    project::get(state.inner(), &id)
}

#[tauri::command]
pub fn create_project(state: State<'_, AppState>, input: CreateProjectInput) -> BiResult<Project> {
    project::create(state.inner(), input)
}

#[tauri::command]
pub fn delete_project(state: State<'_, AppState>, id: String) -> BiResult<()> {
    project::delete(state.inner(), &id)
}

#[tauri::command]
pub fn ensure_project_marker_files(
    state: State<'_, AppState>,
    project_id: String,
) -> BiResult<project::EnsureMarkerFilesResult> {
    project::ensure_marker_files(state.inner(), &project_id)
}

#[derive(Deserialize)]
pub struct IngestArgs {
    pub project_id: String,
    pub root_path: String,
}

#[derive(Deserialize)]
pub struct MultiIngestArgs {
    pub projects: Vec<(String, String)>,
}

#[derive(Serialize)]
pub struct IngestJobResponse {
    pub job_id: String,
    pub project_id: String,
}

#[derive(Serialize, Clone)]
pub struct IngestDone {
    pub job_id: String,
    pub project_id: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub edges_created: usize,
    pub elapsed_ms: u64,
}

#[derive(Serialize, Clone)]
pub struct IngestError {
    pub job_id: String,
    pub project_id: String,
    pub error: String,
}

#[derive(Serialize, Clone)]
pub struct MultiIngestDone {
    pub job_id: String,
    pub total_files_indexed: usize,
    pub total_chunks_indexed: usize,
    pub total_edges_created: usize,
    pub elapsed_ms: u64,
    pub results: Vec<ingest::IngestResult>,
}

#[tauri::command]
pub fn ingest_project(state: State<'_, AppState>, args: IngestArgs) -> BiResult<IngestJobResponse> {
    let root = std::path::PathBuf::from(&args.root_path);
    let operation = crate::operations::start_ingest(state.inner(), &args.project_id, &root)?;

    Ok(IngestJobResponse {
        job_id: operation.id,
        project_id: args.project_id,
    })
}

#[tauri::command]
pub fn start_ingest(
    state: State<'_, AppState>,
    args: IngestArgs,
) -> BiResult<crate::operations::Operation> {
    crate::operations::start_ingest(
        state.inner(),
        &args.project_id,
        std::path::Path::new(&args.root_path),
    )
}

#[tauri::command]
pub fn ingest_multiple_projects(
    state: State<'_, AppState>,
    args: MultiIngestArgs,
) -> BiResult<IngestJobResponse> {
    let projects: Vec<(String, std::path::PathBuf)> = args
        .projects
        .into_iter()
        .map(|(project_id, root_path)| (project_id, std::path::PathBuf::from(root_path)))
        .collect();
    let operation = crate::operations::start_multi_ingest(state.inner(), projects)?;

    Ok(IngestJobResponse {
        job_id: operation.id,
        project_id: "multiple".to_string(),
    })
}

#[derive(Deserialize)]
pub struct GraphArgs {
    pub project_id: String,
}

#[tauri::command]
pub fn get_project_graph(
    state: State<'_, AppState>,
    args: GraphArgs,
) -> BiResult<ingest::GraphData> {
    ingest::get_project_graph(state.inner(), &args.project_id)
}

/// Enqueue a consolidate job. Returns immediately; the UI observes progress
/// through `consolidate_status` and the `consolidate:done` Tauri event.
#[tauri::command]
pub fn consolidate_now(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> BiResult<ConsolidateStatus> {
    crate::operations::start_consolidate(state.inner(), project_id.as_deref())?;
    let mut status = crate::scheduler::get_status();
    status.queued = true;
    Ok(status)
}

#[tauri::command]
pub fn operation_status(
    state: State<'_, AppState>,
    id: String,
) -> BiResult<crate::operations::Operation> {
    crate::operations::get(state.inner(), &id)
}

#[tauri::command]
pub fn list_operations(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> BiResult<Vec<crate::operations::Operation>> {
    crate::operations::list(state.inner(), limit.unwrap_or(100))
}

#[tauri::command]
pub fn cancel_operation(
    state: State<'_, AppState>,
    id: String,
) -> BiResult<crate::operations::Operation> {
    crate::operations::request_cancel(state.inner(), &id)
}

#[tauri::command]
pub fn retry_operation(
    state: State<'_, AppState>,
    id: String,
) -> BiResult<crate::operations::Operation> {
    crate::operations::retry(state.inner(), &id)
}

#[tauri::command]
pub fn consolidate_status(
    _state: State<'_, AppState>,
) -> BiResult<crate::scheduler::ConsolidateStatus> {
    Ok(crate::scheduler::get_status())
}

#[derive(Deserialize)]
pub struct ImportArgs {
    pub project_id: String,
    pub root_path: String,
}

#[tauri::command]
pub fn import_folder(
    state: State<'_, AppState>,
    args: ImportArgs,
) -> BiResult<crate::io::ImportResult> {
    project::get(state.inner(), &args.project_id).map_err(|_| {
        crate::error::BiError::Invalid(format!(
            "project '{}' does not exist — create it first",
            args.project_id
        ))
    })?;
    let root = std::path::Path::new(&args.root_path);
    if !root.exists() {
        return Err(crate::error::BiError::Invalid(format!(
            "root_path '{}' does not exist on disk",
            args.root_path
        )));
    }
    crate::io::import_folder(state.inner(), &args.project_id, root)
}

#[derive(Deserialize)]
pub struct ExportArgs {
    pub project_id: Option<String>,
    pub output_path: String,
}

#[tauri::command]
pub fn export_memories(
    state: State<'_, AppState>,
    args: ExportArgs,
) -> BiResult<crate::io::ExportResult> {
    crate::io::export_memories(
        state.inner(),
        args.project_id.as_deref(),
        std::path::Path::new(&args.output_path),
    )
}

#[derive(Deserialize)]
pub struct WatchArgs {
    pub project_id: String,
    pub root_path: Option<String>,
    pub enabled: bool,
}

#[tauri::command]
pub fn set_watch(state: State<'_, AppState>, args: WatchArgs) -> BiResult<crate::io::WatchStatus> {
    if args.enabled {
        let root =
            if let Some(r) = args.root_path.as_ref() {
                std::path::PathBuf::from(r)
            } else {
                let conn = state.db.conn()?;
                let root: Option<String> = conn
                    .query_row(
                        "SELECT root_path FROM projects WHERE id = ?1",
                        rusqlite::params![&args.project_id],
                        |r| r.get(0),
                    )
                    .ok()
                    .flatten();
                std::path::PathBuf::from(root.ok_or_else(|| {
                    crate::error::BiError::Invalid("no root_path on project".into())
                })?)
            };
        crate::io::enable_watch(state.inner(), &args.project_id, &root)?;
    } else {
        crate::io::disable_watch(state.inner(), &args.project_id)?;
    }
    Ok(crate::io::watch_status())
}

#[tauri::command]
pub fn watch_status() -> crate::io::WatchStatus {
    crate::io::watch_status()
}

#[derive(Deserialize)]
pub struct SetModelArgs {
    pub project_id: String,
    pub model: Option<String>,
}

#[tauri::command]
pub fn set_project_embed_model(state: State<'_, AppState>, args: SetModelArgs) -> BiResult<()> {
    crate::io::set_project_embed_model(state.inner(), &args.project_id, args.model.as_deref())
}

#[tauri::command]
pub fn stats(state: State<'_, AppState>) -> BiResult<Stats> {
    crate::application::stats(state.inner())
}

#[tauri::command]
pub fn bootstrap(state: State<'_, AppState>) -> BiResult<Bootstrap> {
    crate::application::bootstrap(state.inner())
}

#[tauri::command]
pub fn list_agents(state: State<'_, AppState>) -> BiResult<Vec<AgentEntry>> {
    crate::application::list_agents(state.inner())
}

#[derive(Deserialize)]
pub struct RegisterAgentArgs {
    pub name: String,
    pub kind: String,
    pub meta: Option<serde_json::Value>,
}

#[tauri::command]
pub fn register_agent(state: State<'_, AppState>, args: RegisterAgentArgs) -> BiResult<AgentEntry> {
    crate::application::register_agent(state.inner(), args.name, args.kind, args.meta)
}

#[tauri::command]
pub fn recent_activity(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> BiResult<Vec<ActivityEntry>> {
    crate::application::recent_activity(state.inner(), limit.unwrap_or(100))
}

// ── MCP config auto-install ─────────────────────────────────────────────

#[derive(Serialize)]
pub struct ResolveMcpBinaryResult {
    pub path: String,
    pub is_absolute: bool,
}

/// Resolve the absolute path to the bundled `biturbo-mcp` binary.
/// Tries: current_exe parent dir → dev build paths → bare name fallback.
#[tauri::command]
pub fn resolve_mcp_binary_path() -> ResolveMcpBinaryResult {
    let exe_name = if cfg!(windows) {
        "biturbo-mcp.exe"
    } else {
        "biturbo-mcp"
    };

    // 1. Look next to the running app binary (installed builds)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join(exe_name);
            if candidate.exists() {
                return ResolveMcpBinaryResult {
                    path: candidate.to_string_lossy().to_string(),
                    is_absolute: true,
                };
            }
            // macOS .app bundle: exe is Contents/MacOS/biTurbo, binary is alongside
            // Already covered by parent.join above.
        }
    }

    // 2. Dev build paths
    for rel in &["src-tauri/target/release", "src-tauri/target/debug"] {
        if let Ok(cwd) = std::env::current_dir() {
            let candidate = cwd.join(rel).join(exe_name);
            if candidate.exists() {
                return ResolveMcpBinaryResult {
                    path: candidate.to_string_lossy().to_string(),
                    is_absolute: true,
                };
            }
        }
    }

    // 3. Fallback: bare name
    ResolveMcpBinaryResult {
        path: "biturbo-mcp".to_string(),
        is_absolute: false,
    }
}

#[derive(Deserialize)]
pub struct InstallMcpConfigArgs {
    pub target: String,
}

#[derive(Serialize)]
pub struct InstallMcpConfigResult {
    pub target: String,
    pub path: String,
    pub created: bool,
    pub merged: bool,
}

/// Install or merge biTurbo's MCP config into an agent's config file.
/// Non-destructive: preserves existing servers, only adds/overwrites the `biturbo` entry.
#[tauri::command]
pub fn install_mcp_config(
    _state: State<'_, AppState>,
    args: InstallMcpConfigArgs,
) -> BiResult<InstallMcpConfigResult> {
    let bin = resolve_mcp_binary_path();
    let bin_path = &bin.path;

    let home = dirs::home_dir()
        .ok_or_else(|| crate::error::BiError::Invalid("cannot resolve home directory".into()))?;

    let (config_path, format): (std::path::PathBuf, &str) = match args.target.as_str() {
        "cursor" => (home.join(".cursor").join("mcp.json"), "json-cursor"),
        "windsurf" => (
            home.join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            "json-cursor",
        ),
        "claude" => (home.join(".claude.json"), "json-cursor"),
        "opencode" => {
            let base = if cfg!(target_os = "macos") {
                home.join("Library")
                    .join("Application Support")
                    .join("opencode")
            } else {
                home.join(".config").join("opencode")
            };
            (base.join("opencode.json"), "json-opencode")
        }
        "codex" => (home.join(".codex").join("config.toml"), "toml-codex"),
        other => {
            return Err(crate::error::BiError::Invalid(format!(
                "unknown target: {other}"
            )))
        }
    };

    // Create parent dirs
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            crate::error::BiError::Invalid(format!("failed to create {}: {e}", parent.display()))
        })?;
    }

    let existed = config_path.exists();

    match format {
        "json-cursor" => {
            // Cursor/Windsurf/Claude: { "mcpServers": { "biturbo": { ... } } }
            let mut root: serde_json::Value = if existed {
                let content = std::fs::read_to_string(&config_path).map_err(|e| {
                    crate::error::BiError::Invalid(format!(
                        "failed to read {}: {e}",
                        config_path.display()
                    ))
                })?;
                serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            // Ensure mcpServers object exists
            if root.get("mcpServers").is_none() {
                root["mcpServers"] = serde_json::json!({});
            }

            // Add/overwrite biturbo entry, preserve others
            root["mcpServers"]["biturbo"] = serde_json::json!({
                "command": bin_path,
                "args": [],
                "env": {}
            });

            let output = serde_json::to_string_pretty(&root).map_err(|e| {
                crate::error::BiError::Invalid(format!("failed to serialize JSON: {e}"))
            })?;
            std::fs::write(&config_path, output).map_err(|e| {
                crate::error::BiError::Invalid(format!(
                    "failed to write {}: {e}",
                    config_path.display()
                ))
            })?;
        }
        "json-opencode" => {
            // OpenCode: { "mcp": { "biturbo": { "type": "local", "command": [...], ... } } }
            let mut root: serde_json::Value = if existed {
                let content = std::fs::read_to_string(&config_path).map_err(|e| {
                    crate::error::BiError::Invalid(format!(
                        "failed to read {}: {e}",
                        config_path.display()
                    ))
                })?;
                serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            if root.get("mcp").is_none() {
                root["mcp"] = serde_json::json!({});
            }

            root["mcp"]["biturbo"] = serde_json::json!({
                "type": "local",
                "command": [bin_path],
                "enabled": true
            });

            let output = serde_json::to_string_pretty(&root).map_err(|e| {
                crate::error::BiError::Invalid(format!("failed to serialize JSON: {e}"))
            })?;
            std::fs::write(&config_path, output).map_err(|e| {
                crate::error::BiError::Invalid(format!(
                    "failed to write {}: {e}",
                    config_path.display()
                ))
            })?;
        }
        "toml-codex" => {
            // Codex: ~/.codex/config.toml — [mcp_servers.biturbo] table
            // Simple text manipulation: remove existing [mcp_servers.biturbo] block, append new one.
            let biturbo_block =
                format!("[mcp_servers.biturbo]\ncommand = \"{bin_path}\"\nargs = []\n");

            let content = if existed {
                std::fs::read_to_string(&config_path).unwrap_or_default()
            } else {
                String::new()
            };

            // Remove existing [mcp_servers.biturbo] section (from header line to next section or EOF)
            let mut new_content = String::new();
            let mut skip = false;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') && !trimmed.starts_with("[mcp_servers.biturbo") {
                    skip = false;
                }
                if trimmed == "[mcp_servers.biturbo]"
                    || trimmed.starts_with("[mcp_servers.biturbo]")
                {
                    skip = true;
                    continue;
                }
                if !skip {
                    new_content.push_str(line);
                    new_content.push('\n');
                }
            }

            // Ensure there's a blank line before our block if content doesn't end with one
            if !new_content.is_empty() && !new_content.ends_with("\n\n") {
                new_content.push('\n');
            }
            new_content.push_str(&biturbo_block);

            std::fs::write(&config_path, new_content).map_err(|e| {
                crate::error::BiError::Invalid(format!(
                    "failed to write {}: {e}",
                    config_path.display()
                ))
            })?;
        }
        _ => unreachable!(),
    }

    Ok(InstallMcpConfigResult {
        target: args.target,
        path: config_path.to_string_lossy().to_string(),
        created: !existed,
        merged: existed,
    })
}

#[derive(Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub body: String,
    pub available: bool,
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;

    match update {
        Some(update) => Ok(UpdateInfo {
            version: update.version.clone(),
            body: update.body.clone().unwrap_or_default(),
            available: true,
        }),
        None => Ok(UpdateInfo {
            version: String::new(),
            body: String::new(),
            available: false,
        }),
    }
}

#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;

    if let Some(update) = update {
        update
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        app.restart();
    }

    Ok(())
}
