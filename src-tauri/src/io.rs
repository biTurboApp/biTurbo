use crate::db::log_activity;
use crate::error::{BiError, BiResult};
use crate::state::AppState;
use ignore::WalkBuilder;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImportResult {
    pub files_imported: usize,
    pub memories_created: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExportResult {
    pub memories_written: usize,
    pub output_path: String,
}

pub fn import_folder(state: &AppState, project_id: &str, root: &Path) -> BiResult<ImportResult> {
    let mut result = ImportResult::default();

    let files: Vec<PathBuf> = WalkBuilder::new(root)
        .standard_filters(true)
        .git_ignore(true)
        .git_global(true)
        .hidden(false)
        .build()
        .filter_map(|r| r.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let p = e.path().to_path_buf();
            let ext = p.extension()?.to_str()?;
            if matches!(
                ext.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "txt" | "org"
            ) {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("md");
        let chunks = chunk_markdown(&source);
        for (i, chunk_text) in chunks.into_iter().enumerate() {
            if chunk_text.trim().is_empty() {
                continue;
            }
            let input = crate::memory::RememberInput {
                content: chunk_text,
                mem_type: Some("fact".to_string()),
                project_id: Some(project_id.to_string()),
                tags: Some(vec!["imported".to_string(), format!("md:{ext}")]),
                importance: Some(0.5),
                source_agent: Some("import_folder".to_string()),
                file_path: Some(path.to_string_lossy().to_string()),
                ..Default::default()
            };
            match crate::memory::remember(state, input) {
                Ok(_) => result.memories_created += 1,
                Err(e) => result.errors.push(format!("{rel} chunk {i}: {e}")),
            }
        }
        result.files_imported += 1;
    }

    state.db.write(|tx| {
        log_activity(
            tx,
            Some(project_id),
            Some("import_folder"),
            "import",
            None,
            Some(&serde_json::json!({
                "files": result.files_imported,
                "memories": result.memories_created,
            })),
        )?;
        Ok(())
    })?;

    Ok(result)
}

fn chunk_markdown(content: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    const MAX_CHUNK: usize = 1500;

    for line in content.lines() {
        if (line.starts_with("# ") || line.starts_with("## ") || line.starts_with("### "))
            && !current.trim().is_empty()
        {
            chunks.push(current.trim().to_string());
            current.clear();
            current_len = 0;
        }
        current.push_str(line);
        current.push('\n');
        current_len += line.len() + 1;
        if current_len > MAX_CHUNK {
            chunks.push(current.trim().to_string());
            current.clear();
            current_len = 0;
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    if chunks.is_empty() && !content.is_empty() {
        chunks.push(content.to_string());
    }
    chunks
}

pub fn export_memories(
    state: &AppState,
    project_id: Option<&str>,
    output_path: &Path,
) -> BiResult<ExportResult> {
    let mems = crate::memory::list(state, project_id, None, 1_000_000, 0)?;
    let json = serde_json::to_string_pretty(&serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "exported_at": chrono::Utc::now().timestamp_millis(),
        "project_id": project_id,
        "memories": mems,
    }))?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(output_path, json)
        .map_err(|e| BiError::Io(format!("write {}: {e}", output_path.display())))?;
    Ok(ExportResult {
        memories_written: mems.len(),
        output_path: output_path.to_string_lossy().to_string(),
    })
}

pub fn set_project_embed_model(
    state: &AppState,
    project_id: &str,
    model: Option<&str>,
) -> BiResult<()> {
    crate::operations::start_model_rebuild(state, project_id, model)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WatchStatus {
    pub enabled_projects: usize,
    pub watching: Vec<String>,
}

#[derive(Default)]
struct WatchState {
    running: bool,
    queued: bool,
}

type WatchHandle = Arc<Mutex<Option<notify::RecommendedWatcher>>>;
type WatchJobState = Arc<Mutex<WatchState>>;
static WATCHERS: once_cell::sync::Lazy<
    parking_lot::RwLock<std::collections::HashMap<String, (WatchHandle, WatchJobState)>>,
> = once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(std::collections::HashMap::new()));

pub fn enable_watch(state: &AppState, project_id: &str, root: &Path) -> BiResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        tx.execute(
            "UPDATE projects SET watch_enabled = 1, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, project_id],
        )?;
        Ok(())
    })?;
    spawn_watcher(state, project_id, root);
    Ok(())
}

pub fn disable_watch(_state: &AppState, project_id: &str) -> BiResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    _state.db.write(|tx| {
        tx.execute(
            "UPDATE projects SET watch_enabled = 0, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, project_id],
        )?;
        Ok(())
    })?;
    WATCHERS.write().remove(project_id);
    Ok(())
}

pub fn watch_status() -> WatchStatus {
    let g = WATCHERS.read();
    let watching: Vec<String> = g.keys().cloned().collect();
    WatchStatus {
        enabled_projects: watching.len(),
        watching,
    }
}

fn spawn_watcher(state: &AppState, project_id: &str, root: &Path) {
    let project_id_owned = project_id.to_string();
    let root_owned = root.to_path_buf();
    let state_for_cb: Arc<AppState> = Arc::new(state.clone());
    let job_state: WatchJobState = Arc::new(Mutex::new(WatchState::default()));
    let job_state_for_cb = job_state.clone();

    let pid_for_event = project_id_owned.clone();
    let root_for_event = root_owned.clone();
    let state_for_event = state_for_cb.clone();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if !matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                ) {
                    return;
                }

                let mut state = job_state_for_cb.lock();
                if state.running {
                    state.queued = true;
                    return;
                }
                state.running = true;
                drop(state);

                let state_clone = state_for_event.clone();
                let pid = pid_for_event.clone();
                let root = root_for_event.clone();
                let job_state_for_task = job_state_for_cb.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = crate::operations::run_watch_ingest_blocking(&state_clone, &pid, &root);
                    let mut state = job_state_for_task.lock();
                    state.running = false;
                    if state.queued {
                        state.queued = false;
                        state.running = true;
                        drop(state);
                        let run =
                            crate::operations::run_watch_ingest_blocking(&state_clone, &pid, &root);
                        if let Err(e) = &run {
                            tracing::error!("watcher ingest for '{}' failed: {e}", pid);
                        }
                        let mut state = job_state_for_task.lock();
                        state.running = false;
                    }
                });
            }
        }) {
            Ok(w) => w,
            Err(_) => return,
        };

    use notify::Watcher;
    let _ = watcher.watch(root, notify::RecursiveMode::Recursive);
    WATCHERS.write().insert(
        project_id_owned,
        (Arc::new(Mutex::new(Some(watcher))), job_state),
    );
}

pub fn resume_watches(state: &AppState) {
    let Ok(conn) = state.db.conn() else {
        return;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, root_path FROM projects WHERE watch_enabled = 1 AND root_path IS NOT NULL",
    ) else {
        return;
    };
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .ok()
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default();
    for (id, root) in rows {
        if !WATCHERS.read().contains_key(&id) {
            spawn_watcher(state, &id, Path::new(&root));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project;

    fn temp_data_dir() -> PathBuf {
        std::env::temp_dir().join(format!("biturbo-io-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn importing_folder_counts_each_memory_once() {
        let data_dir = temp_data_dir();
        let root = data_dir.join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("one.md"), "# Durable note\nOnly one chunk.").unwrap();
        let state = AppState::open(&data_dir).unwrap();

        let imported = import_folder(&state, &state.default_project_id, &root).unwrap();
        let p = project::get(&state, &state.default_project_id).unwrap();
        assert_eq!(p.memory_count as usize, imported.memories_created);

        std::fs::remove_dir_all(data_dir).ok();
    }

    #[test]
    fn disabling_watch_persists_across_state_reopen() {
        let data_dir = temp_data_dir();
        let root = data_dir.join("watched");
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::open(&data_dir).unwrap();
        let project_id = state.default_project_id.clone();

        enable_watch(&state, &project_id, &root).unwrap();
        assert!(project::get(&state, &project_id).unwrap().watch_enabled);
        disable_watch(&state, &project_id).unwrap();
        assert!(!project::get(&state, &project_id).unwrap().watch_enabled);

        let reopened = AppState::open(&data_dir).unwrap();
        assert!(!project::get(&reopened, &project_id).unwrap().watch_enabled);
        std::fs::remove_dir_all(data_dir).ok();
    }
}
