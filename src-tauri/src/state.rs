//! Shared application state. Lives in the Tauri-managed container; cloned into
//! background tasks. The standalone MCP binary builds its own from the same data dir.

use crate::db::Db;
use crate::embed::Embedder;
use crate::error::{BiError, BiResult};
use crate::index_engine::ProjectIndex;
use parking_lot::Mutex;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Max bytes of index files to keep loaded in memory at once.
/// turbovec keeps the full quantized index in RAM, so this directly
/// caps RSS. 512 MiB is enough for several large projects.
const DEFAULT_INDEX_BUDGET: u64 = 512 * 1024 * 1024;

pub struct AppState {
    pub data_dir: PathBuf,
    pub db: Db,
    pub embedder: Arc<Embedder>,
    pub indices: Arc<RwLock<HashMap<String, Arc<ProjectIndex>>>>,
    pub default_project_id: String,
    pub app: Option<AppHandle>,
    pub index_size_cache: parking_lot::Mutex<Option<(Instant, u64)>>,
    index_access_times: Arc<Mutex<HashMap<String, Instant>>>,
    pub index_memory_budget: u64,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            data_dir: self.data_dir.clone(),
            db: self.db.clone(),
            embedder: self.embedder.clone(),
            indices: self.indices.clone(),
            default_project_id: self.default_project_id.clone(),
            app: self.app.clone(),
            index_size_cache: parking_lot::Mutex::new(None),
            index_access_times: self.index_access_times.clone(),
            index_memory_budget: self.index_memory_budget,
        }
    }
}

impl AppState {
    pub fn open(data_dir: &Path) -> BiResult<Self> {
        std::fs::create_dir_all(data_dir).ok();
        let db_path = data_dir.join("biturbo.db");
        let db = Db::open(&db_path)?;

        let embedder = Arc::new(Embedder::new("BGE-small-en")?);

        // Ensure default project exists.
        let conn = db.conn()?;
        let default_id = "default".to_string();
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT OR IGNORE INTO projects(id, name, bit_width, dim, created_at, updated_at)
             VALUES(?1, ?2, 4, ?3, ?4, ?4)",
            rusqlite::params![default_id, "default", embedder.dim as i64, now],
        )?;

        let state = Self {
            data_dir: data_dir.to_path_buf(),
            db,
            embedder,
            indices: Arc::new(RwLock::new(HashMap::new())),
            default_project_id: default_id,
            app: None,
            index_size_cache: parking_lot::Mutex::new(None),
            index_access_times: Arc::new(Mutex::new(HashMap::new())),
            index_memory_budget: DEFAULT_INDEX_BUDGET,
        };

        // Ensure index files exist on disk, but do NOT load them into memory.
        state.refresh_indices()?;

        // Debounced index flusher + LRU evictor. A plain thread (not tokio)
        // so it runs in every consumer of AppState.
        {
            let state_for_thread = state.clone();
            std::thread::Builder::new()
                .name("biturbo-index-flusher".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    // 1) flush dirty indices
                    let snapshot: Vec<Arc<ProjectIndex>> =
                        state_for_thread.indices.read().values().cloned().collect();
                    for idx in snapshot {
                        let _ = idx.maybe_flush(std::time::Duration::from_millis(300), false);
                    }
                    // 2) evict old indices (not touched in 10 min or over budget)
                    let _ =
                        state_for_thread.evict_stale_indices(std::time::Duration::from_secs(600));
                    let _ = state_for_thread.evict_if_over_budget();
                })
                .ok();
        }

        Ok(state)
    }

    /// Ensure index files exist on disk for every project, but do NOT load
    /// them into the in-memory cache. This keeps startup RSS low.
    pub fn refresh_indices(&self) -> BiResult<()> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare("SELECT id, dim, bit_width FROM projects")?;
        let rows: Vec<(String, usize, u8)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)? as usize,
                    r.get::<_, i64>(2)? as u8,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let data_dir = self.data_dir.join("indices");
        std::fs::create_dir_all(&data_dir).ok();
        for (pid, dim, bw) in rows {
            let file_path = data_dir.join(format!("{pid}.tvim"));
            if !file_path.exists() {
                match ProjectIndex::open_or_create(&pid, dim, bw as usize, &data_dir) {
                    Ok(idx) => {
                        let _ = idx.flush();
                    }
                    Err(e) => {
                        // A bad projects row must not abort startup: in release
                        // builds a panic here (panic=abort) bricks every launch.
                        tracing::error!("failed to create index for project '{pid}': {e}");
                        continue;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_or_load_index(&self, project_id: &str) -> BiResult<Arc<ProjectIndex>> {
        // Ids become index filenames, so block traversal-shaped ids at this chokepoint.
        crate::project::validate_project_id(project_id)?;

        // Hold the write lock across the whole load path (#409): re-checking
        // the cache under it makes concurrent first-opens atomic, so two
        // threads can never double-open the same .tvim file.
        let idx = {
            let mut indices = self.indices.write();
            if let Some(cached) = indices.get(project_id).cloned() {
                self.index_access_times
                    .lock()
                    .insert(project_id.to_string(), Instant::now());
                return Ok(cached);
            }
            // Open the one missing file directly without scanning the projects table.
            let conn = self.db.conn()?;
            let row: Option<(i64, i64)> = conn
                .query_row(
                    "SELECT dim, bit_width FROM projects WHERE id = ?1",
                    rusqlite::params![project_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .ok();
            let (dim, bw) = match row {
                Some((d, b)) => (d as usize, b as u8 as usize),
                None => return Err(BiError::NotFound(format!("project {project_id}"))),
            };
            let loaded = Arc::new(ProjectIndex::open_or_create(
                project_id,
                dim,
                bw,
                &self.data_dir.join("indices"),
            )?);
            indices.insert(project_id.to_string(), loaded.clone());
            self.index_access_times
                .lock()
                .insert(project_id.to_string(), Instant::now());
            loaded // write lock released here, before the budget check below
        };
        let _ = self.evict_if_over_budget();
        Ok(idx)
    }

    /// Approximate in-memory bytes of currently loaded indices.
    /// Uses the on-disk .tvim file size as a proxy (turbovec loads the
    /// full quantized data, so the sizes are close).
    fn loaded_index_bytes(&self) -> u64 {
        let indices = self.indices.read();
        let data_dir = self.data_dir.join("indices");
        let mut total = 0u64;
        for pid in indices.keys() {
            let path = data_dir.join(format!("{pid}.tvim"));
            if let Ok(m) = std::fs::metadata(&path) {
                total += m.len();
            }
        }
        total
    }

    /// Evict least-recently-used indices until the loaded set is under budget.
    fn evict_if_over_budget(&self) -> BiResult<()> {
        // Candidates another thread still holds a clone of; excluded so the
        // loop below cannot spin on the same LRU entry forever.
        let mut skipped: HashSet<String> = HashSet::new();
        loop {
            let budget = self.index_memory_budget;
            let used = self.loaded_index_bytes();
            if used <= budget {
                break;
            }
            let lru_pid = {
                let times = self.index_access_times.lock();
                let mut candidates: Vec<(String, Instant)> = times
                    .iter()
                    .filter(|(k, _)| !skipped.contains(*k))
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                candidates.sort_by_key(|(_, v)| *v);
                candidates.into_iter().map(|(k, _)| k).next()
            };
            if let Some(pid) = lru_pid {
                let mut indices = self.indices.write();
                // Two guards (#410): flush before dropping the map's reference
                // so an unflushed index never loses writes, and skip any index
                // whose Arc is still cloned elsewhere (the map itself holds
                // one) — evicting it would detach an instance another thread
                // is using.
                match indices.get(&pid).cloned() {
                    Some(idx) if Arc::strong_count(&idx) == 1 => {
                        let _ = idx.flush();
                        indices.remove(&pid);
                        self.index_access_times.lock().remove(&pid);
                        tracing::info!(
                            "evicted index '{}' to stay under {} MiB budget",
                            pid,
                            budget / 1024 / 1024
                        );
                    }
                    Some(_) => {
                        skipped.insert(pid);
                    }
                    None => {
                        // Stale tracking entry without a loaded index.
                        self.index_access_times.lock().remove(&pid);
                    }
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Evict indices that haven't been touched in `max_age`.
    fn evict_stale_indices(&self, max_age: Duration) -> BiResult<()> {
        let now = Instant::now();
        let to_evict: Vec<String> = {
            let times = self.index_access_times.lock();
            times
                .iter()
                .filter(|(_, &t)| now.duration_since(t) > max_age)
                .map(|(k, _)| k.clone())
                .collect()
        };
        if !to_evict.is_empty() {
            let mut indices = self.indices.write();
            let mut times = self.index_access_times.lock();
            for pid in to_evict {
                // Same guards as evict_if_over_budget (#410): flush before
                // dropping our reference, and leave alone any index whose Arc
                // is still cloned elsewhere.
                if let Some(idx) = indices.get(&pid).cloned() {
                    if Arc::strong_count(&idx) > 1 {
                        continue;
                    }
                    let _ = idx.flush();
                }
                indices.remove(&pid);
                times.remove(&pid);
                tracing::info!("evicted stale index '{}'", pid);
            }
        }
        Ok(())
    }

    /// Total bytes on disk for project index files. Cached for 5s.
    pub fn index_bytes(&self) -> u64 {
        if let Some((when, n)) = *self.index_size_cache.lock() {
            if when.elapsed().as_secs() < 5 {
                return n;
            }
        }
        let n: u64 = walkdir::WalkDir::new(self.data_dir.join("indices"))
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
        *self.index_size_cache.lock() = Some((Instant::now(), n));
        n
    }

    /// Embed text and add to a project's index. Returns the vector length.
    pub fn embed_and_add(&self, project_id: &str, uid: &str, text: &str) -> BiResult<usize> {
        let vec = self.embedder.embed(text)?;
        let idx = self.get_or_load_index(project_id)?;
        idx.add(uid, &vec)?;
        Ok(vec.len())
    }

    /// Flush every dirty project index to disk. Cheap no-op if nothing changed.
    pub fn flush_all_indices(&self) {
        let indices = self.indices.read();
        for idx in indices.values() {
            let _ = idx.maybe_flush(std::time::Duration::from_millis(500), false);
        }
    }

    pub fn embed_and_search(
        &self,
        project_id: &str,
        query: &str,
        k: usize,
        allowlist: Option<&[String]>,
    ) -> BiResult<Vec<crate::index_engine::SearchHit>> {
        self.repair_index_if_needed(project_id)?;
        let vec = self.embedder.embed(query)?;
        let idx = self.get_or_load_index(project_id)?;
        idx.search(&vec, k, allowlist)
    }

    /// Backfill the vector index when SQLite has more active memories than the on-disk index.
    pub fn repair_index_if_needed(&self, project_id: &str) -> BiResult<()> {
        let db_rows: Vec<(String, String)> = {
            let conn = self.db.conn()?;
            let mut stmt = conn.prepare(
                "SELECT uid, content FROM memories WHERE project_id = ?1 AND superseded_by IS NULL",
            )?;
            let rows = stmt.query_map(rusqlite::params![project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };
        let idx = self.get_or_load_index(project_id)?;
        if idx.len() >= db_rows.len() {
            return Ok(());
        }
        let rows: Vec<(String, String)> = db_rows
            .into_iter()
            .filter(|(uid, _)| !idx.contains_uid(uid))
            .collect();
        if rows.is_empty() {
            return Ok(());
        }
        tracing::info!(
            "repairing vector index for '{}': backfilling {} of {} memories",
            project_id,
            rows.len(),
            idx.len() + rows.len()
        );
        const BATCH: usize = 32;
        for chunk in rows.chunks(BATCH) {
            let text_refs: Vec<&str> = chunk.iter().map(|(_, c)| c.as_str()).collect();
            let vecs = self.embedder.embed_batch(&text_refs)?;
            let items: Vec<(String, Vec<f32>)> = chunk
                .iter()
                .zip(vecs)
                .map(|((uid, _), v)| (uid.clone(), v))
                .collect();
            idx.add_batch(&items)?;
        }
        let _ = idx.flush();
        Ok(())
    }
}
