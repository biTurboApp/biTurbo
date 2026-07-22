//! Shared application state. Lives in the Tauri-managed container; cloned into
//! background tasks. The standalone MCP binary builds its own from the same data dir.

use crate::db::Db;
use crate::embed::Embedder;
use crate::error::{BiError, BiResult};
use crate::index_engine::ProjectIndex;
use parking_lot::Mutex;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Max bytes of index files to keep loaded in memory at once.
/// turbovec keeps the full quantized index in RAM, so this directly
/// caps RSS. 512 MiB is enough for several large projects.
const DEFAULT_INDEX_BUDGET: u64 = 512 * 1024 * 1024;
type ProjectEmbedderCache = HashMap<String, (String, Arc<Embedder>)>;

pub struct AppState {
    pub data_dir: PathBuf,
    pub db: Db,
    pub embedder: Arc<Embedder>,
    project_embedders: Arc<RwLock<ProjectEmbedderCache>>,
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
            project_embedders: self.project_embedders.clone(),
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
            project_embedders: Arc::new(RwLock::new(HashMap::new())),
            indices: Arc::new(RwLock::new(HashMap::new())),
            default_project_id: default_id,
            app: None,
            index_size_cache: parking_lot::Mutex::new(None),
            index_access_times: Arc::new(Mutex::new(HashMap::new())),
            index_memory_budget: DEFAULT_INDEX_BUDGET,
        };

        // Every consumer (desktop or MCP) participates in overdue local
        // maintenance. Persisted leases ensure only one process performs it.
        crate::maintenance::spawn(Arc::new(state.clone()));

        // Ensure index files exist on disk, but do NOT load them into memory.
        state.refresh_indices()?;
        state.replay_all_index_mutations()?;
        crate::operations::recover_interrupted(&state)?;

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
                let idx = ProjectIndex::open_or_create(&pid, dim, bw as usize, &data_dir)
                    .expect("create project index");
                let _ = idx.flush();
            }
        }
        Ok(())
    }

    pub fn get_or_load_index(&self, project_id: &str) -> BiResult<Arc<ProjectIndex>> {
        {
            let indices = self.indices.read();
            if let Some(idx) = indices.get(project_id).cloned() {
                self.index_access_times
                    .lock()
                    .insert(project_id.to_string(), Instant::now());
                return Ok(idx);
            }
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
        let idx = Arc::new(ProjectIndex::open_or_create(
            project_id,
            dim,
            bw,
            &self.data_dir.join("indices"),
        )?);
        {
            let mut indices = self.indices.write();
            indices.insert(project_id.to_string(), idx.clone());
            self.index_access_times
                .lock()
                .insert(project_id.to_string(), Instant::now());
        }
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
        loop {
            let budget = self.index_memory_budget;
            let used = self.loaded_index_bytes();
            if used <= budget {
                break;
            }
            let lru_pid = {
                let times = self.index_access_times.lock();
                let mut candidates: Vec<(String, Instant)> =
                    times.iter().map(|(k, v)| (k.clone(), *v)).collect();
                candidates.sort_by_key(|a| a.1);
                candidates.into_iter().map(|(k, _)| k).next()
            };
            if let Some(pid) = lru_pid {
                let mut indices = self.indices.write();
                indices.remove(&pid);
                self.index_access_times.lock().remove(&pid);
                tracing::info!(
                    "evicted index '{}' to stay under {} MiB budget",
                    pid,
                    budget / 1024 / 1024
                );
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
        let vec = self.embedder_for_project(project_id)?.embed(text)?;
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
        let vec = self.embedder_for_project(project_id)?.embed(query)?;
        let idx = self.get_or_load_index(project_id)?;
        idx.search(&vec, k, allowlist)
    }

    /// Backfill the vector index when SQLite has more active memories than the on-disk index.
    pub fn repair_index_if_needed(&self, project_id: &str) -> BiResult<()> {
        self.replay_index_mutations(project_id)?;
        let idx = self.get_or_load_index(project_id)?;

        // The journal stores the expected uid-set digest after every applied
        // mutation. This detects equal-sized stale indexes, unlike count alone.
        let (active_count, expected_digest): (usize, Option<String>) = {
            let conn = self.db.conn()?;
            let count = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE project_id = ?1 AND superseded_by IS NULL",
                rusqlite::params![project_id],
                |r| r.get::<_, i64>(0),
            )? as usize;
            let digest = conn
                .query_row(
                    "SELECT content_digest FROM index_state WHERE project_id = ?1",
                    rusqlite::params![project_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap_or(None);
            (count, digest)
        };

        if idx.len() == active_count
            && expected_digest
                .as_deref()
                .is_some_and(|expected| expected == idx.uid_digest())
        {
            return Ok(());
        }

        let rows: Vec<(String, String)> = {
            let conn = self.db.conn()?;
            let mut stmt = conn.prepare(
                "SELECT uid, content FROM memories WHERE project_id = ?1 AND superseded_by IS NULL",
            )?;
            let rows = stmt.query_map(rusqlite::params![project_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };
        tracing::info!(
            "rebuilding stale vector index for '{}': {} active memories",
            project_id,
            rows.len()
        );
        const BATCH: usize = 32;
        let mut items: Vec<(String, Vec<f32>)> = Vec::with_capacity(rows.len());
        for chunk in rows.chunks(BATCH) {
            let text_refs: Vec<&str> = chunk.iter().map(|(_, c)| c.as_str()).collect();
            let vecs = self
                .embedder_for_project(project_id)?
                .embed_batch(&text_refs)?;
            items.extend(chunk.iter().zip(vecs).map(|((uid, _), v)| (uid.clone(), v)));
        }
        idx.replace_all(&items)?;
        let _ = idx.flush();
        let digest = idx.uid_digest();
        let now = chrono::Utc::now().timestamp_millis();
        self.db.write(|tx| {
            tx.execute(
                "INSERT INTO index_state(project_id, last_applied_mutation, content_digest, verified_at)
                 VALUES(?1, COALESCE((SELECT MAX(id) FROM index_mutations WHERE project_id = ?1), 0), ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET content_digest = excluded.content_digest,
                    last_applied_mutation = excluded.last_applied_mutation,
                    verified_at = excluded.verified_at",
                rusqlite::params![project_id, digest, now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn embedder_for_project(&self, project_id: &str) -> BiResult<Arc<Embedder>> {
        let model = {
            let conn = self.db.conn()?;
            conn.query_row(
                "SELECT COALESCE(embed_model, ?2) FROM projects WHERE id = ?1",
                rusqlite::params![project_id, crate::embed::DEFAULT_MODEL],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| BiError::NotFound(format!("project {project_id}")))?
        };
        if matches!(model.as_str(), "BGE-small-en" | "BGE-small-en-v1.5") {
            return Ok(self.embedder.clone());
        }
        if let Some((cached_model, embedder)) = self.project_embedders.read().get(project_id) {
            if cached_model == &model {
                return Ok(embedder.clone());
            }
        }
        let embedder = Arc::new(Embedder::new(&model)?);
        self.project_embedders
            .write()
            .insert(project_id.to_string(), (model, embedder.clone()));
        Ok(embedder)
    }

    pub fn invalidate_project_embedder(&self, project_id: &str) {
        self.project_embedders.write().remove(project_id);
    }

    pub fn release_idle_embedders(&self) {
        self.embedder.release_if_idle();
        for (_, embedder) in self.project_embedders.read().values() {
            embedder.release_if_idle();
        }
    }
}
