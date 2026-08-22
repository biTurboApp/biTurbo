use crate::db::{self, log_activity};
use crate::error::{BiError, BiResult};
use crate::state::AppState;
use ignore::WalkBuilder;
use once_cell::sync::Lazy;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use streaming_iterator::StreamingIterator;
use tauri::Emitter;
use tracing;
use tree_sitter::{Language, Parser, Query, QueryCursor};

const CHUNK_INSERT_BATCH: usize = 64;
const INDEX_BATCH: usize = 64;
const PROGRESS_EVERY: usize = 16;
const MAX_CHUNK_TEXT: usize = 4000;
/// Group at most this many chunks into one INSERT statement to stay
/// under SQLite's 32 766 bound-variable limit (15 params per chunk).
const SQL_INSERT_CHUNK_LIMIT: usize = 2000;

/// Capped thread pool for parallel file parsing. Tree-sitter parse trees are
/// 10–30× the size of source files, so running on all cores concurrently can
/// consume tens of GB. 3 threads keeps memory bounded while still overlapping
/// I/O and parsing.
static PARSE_POOL: Lazy<rayon::ThreadPool> = Lazy::new(|| {
    rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .thread_name(|i| format!("biturbo-parse-{i}"))
        .build()
        .expect("parse thread pool")
});

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IngestResult {
    pub project_id: String,
    pub files_indexed: usize,
    pub chunks_indexed: usize,
    pub bytes_processed: u64,
    pub languages: BTreeMap<String, usize>,
    pub errors: Vec<String>,
    pub edges_created: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiIngestResult {
    pub results: Vec<IngestResult>,
    pub total_files_indexed: usize,
    pub total_chunks_indexed: usize,
    pub total_bytes_processed: u64,
    pub total_errors: usize,
    pub total_edges_created: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct IngestProgress {
    pub project_id: String,
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub file: Option<String>,
    pub chunks_so_far: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphData {
    pub project_id: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub uid: String,
    pub label: String,
    pub kind: String,
    pub file_path: Option<String>,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub language: Option<String>,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub edge_type: String,
    pub weight: f32,
}

#[derive(Clone)]
struct PendingChunk {
    uid: String,
    code: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    language: String,
    file_uid: String,
}

impl PendingChunk {
    fn embed_text(&self) -> String {
        let mut text = format!(
            "```{}\n// {}:{}-{}\n{}```\n{}",
            self.language, self.file_path, self.start_line, self.end_line, self.code, self.language,
        );
        if text.len() > MAX_CHUNK_TEXT {
            text = text.chars().take(MAX_CHUNK_TEXT).collect();
        }
        text
    }

    fn db_content(&self) -> String {
        let mut text = format!(
            "// {}:{}-{}\n{}",
            self.file_path, self.start_line, self.end_line, self.code,
        );
        if text.len() > MAX_CHUNK_TEXT {
            text = text.chars().take(MAX_CHUNK_TEXT).collect();
        }
        text
    }
}

/// Per-file output of the parallel parse phase.
#[derive(Clone)]
struct ParsedFile {
    rel: String,
    file_uid: String,
    lang: &'static str,
    bytes: u64,
    file_hash: String,
    chunks: Vec<PendingChunk>,
    imports: Vec<String>,
    error: Option<String>,
}

pub fn ingest_project(state: &AppState, project_id: &str, root: &Path) -> BiResult<IngestResult> {
    if !root.is_dir() {
        return Err(BiError::Ingest(format!("not a dir: {}", root.display())));
    }
    let mut result = IngestResult {
        project_id: project_id.to_string(),
        ..Default::default()
    };

    emit_progress(state, project_id, "scanning", 0, 1, None, 0);

    let files: Vec<PathBuf> = WalkBuilder::new(root)
        .follow_links(false)
        .standard_filters(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(false)
        .build()
        .filter_map(|r| r.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| detect_language(e.path()).map(|_| e.path().to_path_buf()))
        .collect();

    let conn = state.db.conn()?;
    let existing_files = db::get_indexed_files(&conn, project_id)?;
    drop(conn);

    let mut by_basename: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for f in &files {
        if let Some(stem) = f.file_stem().and_then(|s| s.to_str()) {
            by_basename
                .entry(stem.to_string())
                .or_default()
                .push(f.clone());
        }
    }

    let structure = build_structure_summary(root)?;
    let summary_text = format!(
        "Project structure ({}):\n{}",
        root.file_name().and_then(|s| s.to_str()).unwrap_or("root"),
        structure
    );
    state.embed_and_add(
        project_id,
        &format!("{project_id}::structure"),
        &summary_text,
    )?;
    {
        let conn = state.db.conn()?;
        db::log_activity(
            &conn,
            Some(project_id),
            None,
            "ingest",
            None,
            Some(&serde_json::json!({"phase": "structure"})),
        )?;
    }

    let total = files.len().max(1);

    // ---- INCREMENTAL: read/hash every file, but parse only changed files.
    let progress = AtomicUsize::new(0);
    let parsed: Vec<ParsedFile> = PARSE_POOL.install(|| {
        files
            .par_iter()
            .map(|path| {
                let done = progress.fetch_add(1, Ordering::Relaxed);
                if done % PROGRESS_EVERY == 0 {
                    emit_progress(
                        state,
                        project_id,
                        "parsing",
                        done,
                        total,
                        Some(path.to_string_lossy().to_string()),
                        0,
                    );
                }
                parse_one_file(project_id, root, path, &existing_files)
            })
            .collect()
    });

    let idx = state.get_or_load_index(project_id)?;
    let mut current_rels: HashSet<String> = HashSet::new();
    let mut file_uids: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_chunks: Vec<&PendingChunk> = Vec::new();
    let mut pending_edges: Vec<(String, String, String, f32)> = Vec::new();
    let mut edge_keys: HashSet<String> = HashSet::new();
    let mut import_sources: HashMap<String, Vec<String>> = HashMap::new();
    let mut changed_rels: Vec<String> = Vec::new();
    let mut changed_file_uids: Vec<String> = Vec::new();
    let mut stale_file_uids: Vec<String> = Vec::new();
    let mut deleted_file_uids: Vec<String> = Vec::new();
    let mut changed_pfs: Vec<ParsedFile> = Vec::new();
    let mut parse_error_rels: Vec<String> = Vec::new();

    for pf in parsed {
        current_rels.insert(pf.rel.clone());
        result.bytes_processed += pf.bytes;

        if let Some(e) = &pf.error {
            result.errors.push(e.clone());
            parse_error_rels.push(pf.rel.clone());
            stale_file_uids.push(pf.file_uid.clone());
            changed_file_uids.push(pf.file_uid.clone());
            changed_pfs.push(pf);
            continue;
        }

        result.files_indexed += 1;
        *result.languages.entry(pf.lang.to_string()).or_insert(0) += 1;
        file_uids.insert(pf.rel.clone(), pf.file_uid.clone());

        let unchanged = existing_files
            .get(&pf.rel)
            .is_some_and(|existing| existing.file_hash == pf.file_hash);
        let imports_for_edges = if unchanged {
            existing_files
                .get(&pf.rel)
                .map(|e| e.imports.clone())
                .unwrap_or_default()
        } else {
            pf.imports.clone()
        };
        for imp in &imports_for_edges {
            if let Some(target_rel) = resolve_import(imp, &root.join(&pf.rel), root, &by_basename) {
                if target_rel != pf.rel {
                    let sources = import_sources.entry(target_rel).or_default();
                    if !sources.contains(&pf.file_uid) {
                        sources.push(pf.file_uid.clone());
                    }
                }
            }
        }

        if unchanged {
            continue;
        }

        changed_rels.push(pf.rel.clone());
        changed_file_uids.push(pf.file_uid.clone());
        changed_pfs.push(pf);
    }

    for rel in existing_files.keys() {
        if !current_rels.contains(rel) {
            deleted_file_uids.push(format!("{project_id}::file::{rel}"));
        }
    }

    let conn = state.db.conn()?;
    let mut stale_uids = Vec::new();
    for rel in changed_rels.iter().chain(parse_error_rels.iter()) {
        stale_uids.extend(db::code_uids_for_file(&conn, project_id, rel)?);
    }
    for rel in existing_files
        .keys()
        .filter(|rel| !current_rels.contains(*rel))
    {
        stale_uids.extend(db::code_uids_for_file(&conn, project_id, rel)?);
    }
    drop(conn);

    pending_chunks.extend(changed_pfs.iter().flat_map(|pf| pf.chunks.iter()));

    for uid in &stale_uids {
        let _ = idx.remove(uid);
    }

    // ---- STREAMED: embed changed chunks in small ONNX batches and write the
    // vector index in small batches so peak RAM stays bounded.
    let total_chunks = pending_chunks.len();
    let mut embedded_so_far = 0;

    for wave in pending_chunks.chunks(CHUNK_INSERT_BATCH) {
        let embed_texts: Vec<String> = wave.iter().map(|c| c.embed_text()).collect();
        let embed_refs: Vec<&str> = embed_texts.iter().map(String::as_str).collect();
        let mut wave_offset = 0usize;
        let mut index_items: Vec<(String, Vec<f32>)> = Vec::with_capacity(INDEX_BATCH);

        state
            .embedder
            .embed_batch_uncached_stream(&embed_refs, |chunk_texts, embeddings| {
                for (i, emb) in embeddings.into_iter().enumerate() {
                    let c = &wave[wave_offset + i];
                    index_items.push((c.uid.clone(), emb));
                    if index_items.len() >= INDEX_BATCH {
                        idx.add_batch(&index_items)?;
                        index_items.clear();
                    }
                }
                wave_offset += chunk_texts.len();
                Ok(())
            })?;

        if !index_items.is_empty() {
            idx.add_batch(&index_items)?;
        }

        for c in wave {
            let key = format!("{}\0{}\0member_of", c.uid, c.file_uid);
            if edge_keys.insert(key) {
                pending_edges.push((c.uid.clone(), c.file_uid.clone(), "member_of".into(), 1.0));
            }
        }

        embedded_so_far += wave.len();
        emit_progress(
            state,
            project_id,
            "embedding",
            embedded_so_far,
            total_chunks,
            None,
            embedded_so_far,
        );
    }
    let _ = idx.flush();

    // ---- SQLite: delete stale chunks for changed/deleted files, insert new
    // chunks, rebuild changed file edges, and update indexed_files metadata.
    emit_progress(
        state,
        project_id,
        "writing",
        total,
        total,
        None,
        result.chunks_indexed,
    );

    drop(pending_chunks);

    let mut valid_changed_pfs: Vec<ParsedFile> = Vec::new();
    for pf in &changed_pfs {
        if pf.error.is_none() {
            valid_changed_pfs.push(pf.clone());
        }
    }

    for pf in &changed_pfs {
        result.chunks_indexed += pf.chunks.len();
    }

    for pf in &valid_changed_pfs {
        let abs = root.join(&pf.rel);
        for imp in &pf.imports {
            if let Some(target_rel) = resolve_import(imp, &abs, root, &by_basename) {
                if target_rel == pf.rel {
                    continue;
                }
                if let Some(target_file_uid) = file_uids.get(&target_rel) {
                    let key = format!("{}\0{}\0imports", pf.file_uid, target_file_uid);
                    if edge_keys.insert(key) {
                        pending_edges.push((
                            pf.file_uid.clone(),
                            target_file_uid.clone(),
                            "imports".into(),
                            1.0,
                        ));
                    }
                }
            }
        }
    }

    for rel in &changed_rels {
        if let Some(target_file_uid) = file_uids.get(rel) {
            if let Some(sources) = import_sources.get(rel) {
                for source_uid in sources {
                    if source_uid == target_file_uid {
                        continue;
                    }
                    let key = format!("{}\0{}\0imports", source_uid, target_file_uid);
                    if edge_keys.insert(key) {
                        pending_edges.push((
                            source_uid.clone(),
                            target_file_uid.clone(),
                            "imports".into(),
                            1.0,
                        ));
                    }
                }
            }
        }
    }

    state.db.write(|tx| {
        let now = chrono::Utc::now().timestamp_millis();
        db::delete_memories_by_uids(tx, &stale_uids)?;
        db::delete_code_edges_for_files(tx, project_id, &changed_file_uids)?;
        db::delete_code_edges_for_files(tx, project_id, &stale_file_uids)?;
        db::delete_code_edges_for_files(tx, project_id, &deleted_file_uids)?;

        for rel in existing_files
            .keys()
            .filter(|rel| !current_rels.contains(*rel))
            .chain(parse_error_rels.iter())
        {
            db::delete_indexed_file(tx, project_id, rel)?;
        }

        // Batch inserts by cumulative chunk count (not file count) to stay
        // under SQLite's 32 766 host-parameter limit.
        {
            let mut group: Vec<&ParsedFile> = Vec::new();
            let mut group_chunks: usize = 0;
            for pf in &valid_changed_pfs {
                let n = pf.chunks.len();
                if group_chunks + n > SQL_INSERT_CHUNK_LIMIT && group_chunks > 0 {
                    flush_chunk_insert(
                        tx, project_id, &group, group_chunks, now,
                    )?;
                    group.clear();
                    group_chunks = 0;
                }
                group.push(pf);
                group_chunks += n;
            }
            if group_chunks > 0 {
                flush_chunk_insert(
                    tx, project_id, &group, group_chunks, now,
                )?;
            }
        }

        for pf in &valid_changed_pfs {
            db::upsert_indexed_file(
                tx,
                project_id,
                &pf.rel,
                &pf.file_hash,
                pf.lang,
                &pf.imports,
                now,
            )?;
        }

        if !pending_edges.is_empty() {
            for batch in pending_edges.chunks(512) {
                let n = batch.len();
                let placeholders = vec!["(?,?,?,?,?,?)"; n].join(",");
                let sql = format!(
                    "INSERT INTO code_edges(project_id, from_uid, to_uid, edge_type, weight, created_at) VALUES {placeholders}"
                );
                let mut stmt = tx.prepare_cached(&sql)?;
                let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(n * 6);
                for (from, to, etype, weight) in batch {
                    params.push(project_id.to_string().into());
                    params.push(from.clone().into());
                    params.push(to.clone().into());
                    params.push(etype.clone().into());
                    params.push((*weight as f64).into());
                    params.push(now.into());
                }
                stmt.execute(rusqlite::params_from_iter(params))?;
            }
        }

        let total_code_chunks: i64 = tx.query_row(
            "SELECT COUNT(*) FROM memories WHERE project_id = ?1 AND mem_type = 'code'",
            rusqlite::params![project_id],
            |r| r.get(0),
        )?;
        result.chunks_indexed = total_code_chunks as usize;
        result.edges_created = pending_edges.len();

        tx.execute(
            "UPDATE projects SET indexed_count = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![result.chunks_indexed as i64, now, project_id],
        )?;
        log_activity(
            tx,
            Some(project_id),
            None,
            "ingest",
            None,
            Some(&serde_json::json!({
                "files": result.files_indexed,
                "chunks": result.chunks_indexed,
                "edges": result.edges_created,
                "changed_files": changed_rels.len(),
                "deleted_files": existing_files.keys().filter(|rel| !current_rels.contains(*rel)).count(),
            })),
        )?;
        Ok(())
    })?;

    state.embedder.force_release();

    Ok(result)
}

/// Ingest multiple projects sequentially. Per-project parsing still uses a
/// capped Rayon pool, but project-level parallelism causes ONNX/CPU contention
/// and large memory spikes.
/// Progress events include project_id to distinguish which project is being processed.
pub fn ingest_multiple_projects(
    state: &AppState,
    projects: Vec<(String, PathBuf)>,
) -> BiResult<MultiIngestResult> {
    let mut results = Vec::with_capacity(projects.len());
    for (project_id, root) in projects {
        results.push(
            ingest_project(state, &project_id, &root)
                .map_err(|e| BiError::Ingest(format!("project {project_id} failed: {e}"))),
        );
    }

    let mut multi_result = MultiIngestResult::default();
    for result in results {
        match result {
            Ok(r) => {
                multi_result.total_files_indexed += r.files_indexed;
                multi_result.total_chunks_indexed += r.chunks_indexed;
                multi_result.total_bytes_processed += r.bytes_processed;
                multi_result.total_errors += r.errors.len();
                multi_result.total_edges_created += r.edges_created;
                multi_result.results.push(r);
            }
            Err(e) => {
                multi_result.total_errors += 1;
                multi_result.results.push(IngestResult {
                    project_id: "unknown".to_string(),
                    errors: vec![e.to_string()],
                    ..Default::default()
                });
            }
        }
    }

    state.embedder.force_release();

    Ok(multi_result)
}

pub fn get_project_graph(state: &AppState, project_id: &str) -> BiResult<GraphData> {
    let conn = state.db.conn()?;
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT uid,
                CASE WHEN instr(content, char(10)) > 0
                     THEN substr(content, 1, instr(content, char(10)) - 1)
                     ELSE content END AS first_line,
                file_path, start_line, end_line, language
         FROM memories
         WHERE project_id = ?1 AND mem_type = 'code'
         ORDER BY file_path, start_line",
    )?;
    type ChunkRow = (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
    );
    let chunks: Vec<ChunkRow> = stmt
        .query_map(rusqlite::params![project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut by_file: BTreeMap<String, Vec<&ChunkRow>> = BTreeMap::new();
    for c in &chunks {
        if let Some(fp) = &c.2 {
            by_file.entry(fp.clone()).or_default().push(c);
        }
    }

    for (file_path, members) in &by_file {
        let file_uid = format!("{project_id}::file::{}", file_path.trim_start_matches('/'));
        let short = file_path
            .rsplit('/')
            .next()
            .unwrap_or(file_path)
            .to_string();
        nodes.push(GraphNode {
            uid: file_uid.clone(),
            label: short,
            kind: "file".into(),
            file_path: Some(file_path.clone()),
            start_line: None,
            end_line: None,
            language: members.first().and_then(|m| m.5.clone()),
            size: members.len(),
        });

        for m in members {
            let label = derive_label(&m.1);
            let content_hint = &m.1;
            let kind = if content_hint.contains("class") || content_hint.contains("Class") {
                "class"
            } else if m.1.contains("struct") || m.1.contains("Struct") {
                "struct"
            } else {
                "function"
            };
            let size = ((m.4.unwrap_or(0) - m.3.unwrap_or(0) + 1).max(1)) as usize;
            nodes.push(GraphNode {
                uid: m.0.clone(),
                label,
                kind: kind.into(),
                file_path: Some(file_path.clone()),
                start_line: m.3,
                end_line: m.4,
                language: m.5.clone(),
                size,
            });
        }
    }

    let mut stmt = conn.prepare(
        "SELECT from_uid, to_uid, edge_type, weight FROM code_edges WHERE project_id = ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_id], |r| {
        Ok(GraphEdge {
            from: r.get::<_, String>(0)?,
            to: r.get::<_, String>(1)?,
            edge_type: r.get::<_, String>(2)?,
            weight: r.get::<_, f64>(3)? as f32,
        })
    })?;
    for r in rows {
        edges.push(r?);
    }

    Ok(GraphData {
        project_id: project_id.to_string(),
        nodes,
        edges,
    })
}

fn derive_label(content_hint: &str) -> String {
    let trimmed = content_hint.trim();
    if trimmed.is_empty() {
        return "(anon)".to_string();
    }
    let first = trimmed.lines().next().unwrap_or(trimmed);
    // Remove common comment prefixes so the label looks cleaner
    let cleaned = first
        .trim_start_matches("//")
        .trim_start_matches("#")
        .trim_start_matches("/*")
        .trim_start_matches("*")
        .trim_start_matches("/**")
        .trim();
    if cleaned.is_empty() {
        return first.chars().take(60).collect();
    }
    cleaned.chars().take(60).collect()
}

fn flush_chunk_insert(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    batch: &[&ParsedFile],
    total_chunks: usize,
    now: i64,
) -> BiResult<()> {
    // Collect uids first, deduped but in insertion order. The explicit
    // DELETE below is required because `INSERT OR REPLACE` removes old rows
    // without firing the memories_ad trigger (recursive_triggers is OFF),
    // which would leave stale duplicate rows in memories_fts forever.
    let mut uids: Vec<&str> = Vec::with_capacity(total_chunks);
    let mut seen = HashSet::new();
    for pf in batch {
        for c in &pf.chunks {
            if seen.insert(c.uid.as_str()) {
                uids.push(&c.uid);
            }
        }
    }
    for group in uids.chunks(500) {
        let placeholders = vec!["?"; group.len()].join(",");
        let sql = format!("DELETE FROM memories WHERE uid IN ({placeholders})");
        tx.prepare_cached(&sql)?
            .execute(rusqlite::params_from_iter(group.iter()))?;
    }
    let placeholders = vec!["(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"; total_chunks].join(",");
    let sql = format!(
        "INSERT INTO memories
           (uid, project_id, mem_type, content, importance,
            created_at, updated_at, last_access, access_count,
            file_path, start_line, end_line, language, tags, source_agent)
         VALUES {placeholders}"
    );
    let mut stmt = tx.prepare_cached(&sql)?;
    let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(total_chunks * 15);
    for pf in batch {
        for c in &pf.chunks {
            params.push(c.uid.clone().into());
            params.push(project_id.to_string().into());
            params.push("code".to_string().into());
            params.push(c.db_content().into());
            params.push(0.5_f64.into());
            params.push(now.into());
            params.push(now.into());
            params.push(now.into());
            params.push(0_i64.into());
            params.push(c.file_path.clone().into());
            params.push(c.start_line.into());
            params.push(c.end_line.into());
            params.push(c.language.clone().into());
            params.push("[]".to_string().into());
            params.push(rusqlite::types::Value::Null);
        }
    }
    stmt.execute(rusqlite::params_from_iter(params))?;
    Ok(())
}

fn detect_language(p: &Path) -> Option<&'static str> {
    let ext = p.extension()?.to_str()?;
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "swift" => "swift",
        "php" => "php",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "sh" | "bash" => "bash",
        "html" | "htm" => "html",
        "css" => "css",
        _ => return None,
    })
}

fn language_for(lang: &str) -> Result<tree_sitter::Language, String> {
    let lang: tree_sitter::Language = match lang {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "ruby" => tree_sitter_ruby::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        _ => return Err(format!("unsupported lang {lang}")),
    };
    Ok(lang)
}

/// Compiled grammar + queries for one language. Built once per process —
/// `Query::new` is expensive and used to run twice per file.
struct LangBundle {
    language: Language,
    chunk_query: Option<Query>,
    import_query: Option<Query>,
}

static LANG_BUNDLES: Lazy<HashMap<&'static str, LangBundle>> = Lazy::new(|| {
    let langs = [
        "rust",
        "typescript",
        "javascript",
        "python",
        "go",
        "swift",
        "php",
        "ruby",
        "java",
        "c",
        "cpp",
        "csharp",
        "bash",
        "html",
        "css",
    ];
    let mut map = HashMap::new();
    for name in langs {
        let Ok(language) = language_for(name) else {
            continue;
        };
        let chunk_query = chunk_query_src(name).and_then(|src| Query::new(&language, src).ok());
        let import_query = import_query_src(name).and_then(|src| Query::new(&language, src).ok());
        map.insert(
            name,
            LangBundle {
                language,
                chunk_query,
                import_query,
            },
        );
    }
    map
});

/// Read, parse (once), and chunk a single file. Runs on a rayon worker.
fn parse_one_file(
    project_id: &str,
    root: &Path,
    path: &Path,
    existing_files: &HashMap<String, db::IndexedFileInfo>,
) -> ParsedFile {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let file_uid = format!("{project_id}::file::{rel}");
    let lang = detect_language(path).unwrap_or("");
    let mut pf = ParsedFile {
        rel,
        file_uid: file_uid.clone(),
        lang,
        bytes: 0,
        file_hash: String::new(),
        chunks: Vec::new(),
        imports: Vec::new(),
        error: None,
    };

    let mut bytes = Vec::new();
    if std::fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .is_err()
    {
        pf.error = Some(format!("{}: unreadable", path.display()));
        return pf;
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    pf.file_hash = hex::encode(hasher.finalize());
    pf.bytes = bytes.len() as u64;

    if existing_files
        .get(&pf.rel)
        .is_some_and(|existing| existing.file_hash == pf.file_hash)
    {
        pf.imports = existing_files
            .get(&pf.rel)
            .map(|e| e.imports.clone())
            .unwrap_or_default();
        return pf;
    }

    let Ok(source) = String::from_utf8(bytes) else {
        pf.error = Some(format!("{}: not utf-8", path.display()));
        return pf;
    };

    let Some(bundle) = LANG_BUNDLES.get(lang) else {
        pf.error = Some(format!("{}: no grammar for {lang}", path.display()));
        return pf;
    };
    let mut parser = Parser::new();
    if parser.set_language(&bundle.language).is_err() {
        pf.error = Some(format!("{}: set_language failed", path.display()));
        return pf;
    }
    let Some(tree) = parser.parse(&source, None) else {
        pf.error = Some(format!("{}: parse failed", path.display()));
        return pf;
    };
    let root_node = tree.root_node();

    let chunks = collect_chunks(bundle, root_node, &source);
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let file_path_str = path.to_string_lossy().to_string();
    for chunk in chunks {
        // Include the end line: minified/bundled files routinely declare many
        // functions on one physical line, so start_line alone collides (#562).
        // collect_chunks dedupes exact (start, end) spans, making this unique.
        let uid = format!(
            "{project_id}::{}::{}-{}",
            pf.rel, chunk.start_line, chunk.end_line
        );
        pf.chunks.push(PendingChunk {
            uid,
            code: chunk.code,
            file_path: file_path_str.clone(),
            start_line: chunk.start_line as i64,
            end_line: chunk.end_line as i64,
            language: lang.to_string(),
            file_uid: file_uid.clone(),
        });
    }

    pf.imports = collect_imports(bundle, root_node, &source);
    let _ = file_name;
    pf
}

#[derive(Debug, Clone)]
struct Chunk {
    code: String,
    start_line: usize,
    end_line: usize,
}

fn chunk_query_src(lang: &str) -> Option<&'static str> {
    let query_src = match lang {
        "rust" => {
            r#"
            (function_item name: (identifier) @name) @def
            (struct_item name: (type_identifier) @name) @def
            (enum_item name: (type_identifier) @name) @def
            (trait_item name: (type_identifier) @name) @def
            (impl_item type: (type_identifier) @name) @def
        "#
        }
        "javascript" => {
            r#"
            (function_declaration name: (identifier) @name) @def
            (class_declaration name: (identifier) @name) @def
            (method_definition name: (property_identifier) @name) @def
            (export_statement declaration: (function_declaration name: (identifier) @name)) @def
        "#
        }
        "typescript" => {
            r#"
            (function_declaration name: (identifier) @name) @def
            (class_declaration name: (type_identifier) @name) @def
            (method_definition name: (property_identifier) @name) @def
            (interface_declaration name: (type_identifier) @name) @def
            (type_alias_declaration name: (type_identifier) @name) @def
            (export_statement declaration: (function_declaration name: (identifier) @name)) @def
        "#
        }
        "python" => {
            r#"
            (function_definition name: (identifier) @name) @def
            (class_definition name: (identifier) @name) @def
        "#
        }
        "go" => {
            r#"
            (function_declaration name: (identifier) @name) @def
            (method_declaration name: (field_identifier) @name) @def
            (type_declaration (type_spec name: (type_identifier) @name)) @def
        "#
        }
        "swift" => {
            r#"
            (function_declaration) @def
            (class_declaration) @def
            (protocol_declaration) @def
        "#
        }
        "php" => {
            r#"
            (function_definition) @def
            (class_declaration) @def
            (interface_declaration) @def
            (trait_declaration) @def
            (method_declaration) @def
        "#
        }
        "ruby" => {
            r#"
            (method) @def
            (class) @def
            (module) @def
        "#
        }
        "java" => {
            r#"
            (method_declaration) @def
            (class_declaration) @def
            (interface_declaration) @def
            (enum_declaration) @def
        "#
        }
        "c" => {
            r#"
            (function_definition) @def
            (struct_specifier) @def
            (union_specifier) @def
            (enum_specifier) @def
        "#
        }
        "cpp" => {
            r#"
            (function_definition) @def
            (class_specifier) @def
            (struct_specifier) @def
            (namespace_definition) @def
        "#
        }
        "csharp" => {
            r#"
            (method_declaration) @def
            (class_declaration) @def
            (interface_declaration) @def
            (struct_declaration) @def
        "#
        }
        "bash" => {
            r#"
            (function_definition) @def
        "#
        }
        "css" => {
            r#"
            (rule_set) @def
        "#
        }
        _ => return None,
    };
    Some(query_src)
}

/// Walk chunk-query matches over an already-parsed tree.
fn collect_chunks(bundle: &LangBundle, root: tree_sitter::Node<'_>, source: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return chunks;
    }
    // Track (start_line, end_line) to deduplicate captures from overlapping
    // query patterns (e.g. TypeScript `export_statement` + `function_declaration`).
    let mut seen_spans: HashSet<(usize, usize)> = HashSet::new();

    if let Some(query) = &bundle.chunk_query {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, source.as_bytes());
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let node = cap.node;
                if !matches!(
                    node.kind(),
                    "function_item"
                        | "struct_item"
                        | "enum_item"
                        | "trait_item"
                        | "impl_item"
                        | "function_declaration"
                        | "class_declaration"
                        | "method_definition"
                        | "interface_declaration"
                        | "type_alias_declaration"
                        | "export_statement"
                        | "function_definition"
                        | "class_definition"
                        | "method_declaration"
                        | "type_declaration"
                ) {
                    continue;
                }
                let start = node.start_position().row;
                let end = node
                    .end_position()
                    .row
                    .min(start + 200)
                    .min(lines.len() - 1);
                let start_line = start + 1;
                let end_line = end + 1;
                if !seen_spans.insert((start_line, end_line)) {
                    tracing::warn!(
                        "ingest: duplicate span L{}-{} skipped (overlapping query patterns)",
                        start_line,
                        end_line
                    );
                    continue; // duplicate span from overlapping query patterns
                }
                let code = lines[start..=end].join("\n");
                let _kind = node.kind().replace('_', " ");
                chunks.push(Chunk {
                    code,
                    start_line,
                    end_line,
                });
            }
        }
    }

    if chunks.is_empty() {
        let cap = (lines.len() - 1).min(200);
        chunks.push(Chunk {
            code: lines[..=cap].join("\n"),
            start_line: 1,
            end_line: cap + 1,
        });
    }

    chunks
}

fn import_query_src(lang: &str) -> Option<&'static str> {
    let query_src = match lang {
        "rust" => {
            r#"
            (use_declaration argument: (_) @imp)
            (mod_item name: (identifier) @imp)
            (extern_crate_declaration name: (identifier) @imp)
        "#
        }
        "javascript" | "typescript" => {
            r#"
            (import_statement source: (string) @imp)
            (export_statement source: (string) @imp)
            (call_expression function: (identifier) @fn arguments: (arguments (string) @imp))
        "#
        }
        "python" => {
            r#"
            (import_statement name: (dotted_name) @imp)
            (import_from_statement module_name: (dotted_name) @imp)
        "#
        }
        "go" => {
            r#"
            (import_spec path: (interpreted_string_literal) @imp)
        "#
        }
        "swift" => {
            r#"
            (import_declaration) @imp
        "#
        }
        "php" => {
            r#"
            (namespace_use_declaration) @imp
        "#
        }
        "java" => {
            r#"
            (import_declaration) @imp
        "#
        }
        "c" | "cpp" => {
            r#"
            (preproc_include) @imp
        "#
        }
        "csharp" => {
            r#"
            (using_directive) @imp
        "#
        }
        "css" => {
            r#"
            (import_statement) @imp
        "#
        }
        _ => return None,
    };
    Some(query_src)
}

/// Walk import-query matches over an already-parsed tree.
fn collect_imports(bundle: &LangBundle, root: tree_sitter::Node<'_>, source: &str) -> Vec<String> {
    let Some(query) = &bundle.import_query else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut imports = Vec::new();
    let mut matches = cursor.matches(query, root, source.as_bytes());
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let node = cap.node;
            if node.kind() == "string" || node.kind() == "interpreted_string_literal" {
                let raw = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let cleaned = raw.trim_matches(|c| c == '"' || c == '\'').to_string();
                if !cleaned.is_empty() {
                    imports.push(cleaned);
                }
            } else if matches!(
                node.kind(),
                "identifier" | "dotted_name" | "scoped_identifier"
            ) {
                let txt = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if !txt.is_empty() {
                    imports.push(txt);
                }
            }
        }
    }
    imports
}

fn resolve_import(
    import: &str,
    from_file: &Path,
    root: &Path,
    by_basename: &BTreeMap<String, Vec<PathBuf>>,
) -> Option<String> {
    let import = import
        .trim_start_matches("crate::")
        .trim_start_matches("self::")
        .trim_start_matches("super::");

    if import.starts_with("./") || import.starts_with("../") || import.starts_with('/') {
        let base = if import.starts_with('/') {
            root.to_path_buf()
        } else {
            from_file.parent()?.to_path_buf()
        };
        let candidate = base.join(import.trim_start_matches('.').trim_start_matches('/'));
        let candidates = expand_candidates(&candidate);
        for c in candidates {
            if c.exists() {
                let rel = c.strip_prefix(root).ok()?.to_string_lossy().to_string();
                return Some(rel);
            }
        }
        return None;
    }

    let first = import.split("::").next().unwrap_or(import);
    let first = first.split('.').next().unwrap_or(first);
    if let Some(matches) = by_basename.get(first) {
        if let Some(first_match) = matches.first() {
            let rel = first_match
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .to_string();
            return Some(rel);
        }
    }
    None
}

fn expand_candidates(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    let exts = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go"];
    for ext in exts {
        out.push(p.with_extension(ext));
    }
    for ext in &exts {
        out.push(p.join(format!("index.{ext}")));
    }
    out
}

fn build_structure_summary(root: &Path) -> BiResult<String> {
    let mut lines: Vec<String> = Vec::new();
    for entry in WalkBuilder::new(root)
        .max_depth(Some(3))
        .follow_links(false)
        .standard_filters(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .hidden(true)
        .build()
        .filter_map(|r| r.ok())
    {
        let p = entry.path();
        let rel = p.strip_prefix(root).unwrap_or(p);
        if rel.components().count() == 0 {
            continue;
        }
        let depth = rel.components().count();
        let indent = "  ".repeat(depth - 1);
        let name = rel
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if p.is_dir() {
            lines.push(format!("{indent}{name}/"));
        } else {
            lines.push(format!("{indent}{name}"));
        }
    }
    if lines.len() > 200 {
        let half = 100;
        let omitted = lines.len() - 2 * half;
        let mut truncated: Vec<String> = lines[..half].to_vec();
        truncated.push(format!("… ({} more entries) …", omitted));
        truncated.extend_from_slice(&lines[lines.len() - half..]);
        lines = truncated;
    }
    Ok(lines.join("\n"))
}

fn emit_progress(
    state: &AppState,
    project_id: &str,
    phase: &str,
    current: usize,
    total: usize,
    file: Option<String>,
    chunks_so_far: usize,
) {
    if let Some(app) = &state.app {
        let _ = app.emit(
            "ingest:progress",
            IngestProgress {
                project_id: project_id.to_string(),
                phase: phase.to_string(),
                current,
                total,
                file,
                chunks_so_far,
            },
        );
    }
}

#[allow(dead_code)]
fn _unused_hashset() -> HashSet<()> {
    HashSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_one_file_extracts_chunks_and_imports() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.rs");
        std::fs::write(
            &file,
            "use std::collections::HashMap;\n\npub struct Thing { x: u32 }\n\npub fn do_it() -> u32 { 42 }\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "rust");
        assert_eq!(pf.rel, "sample.rs");
        assert_eq!(pf.file_uid, "proj::file::sample.rs");
        // struct + fn definitions become chunks.
        assert!(pf.chunks.len() >= 2, "chunks: {}", pf.chunks.len());
        assert!(pf.chunks.iter().any(|c| c.db_content().contains("do_it")));
        // The `use` import is collected.
        assert!(!pf.imports.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lang_bundles_compile_for_all_languages() {
        for lang in [
            "rust",
            "typescript",
            "javascript",
            "python",
            "go",
            "swift",
            "php",
            "ruby",
            "java",
            "c",
            "cpp",
            "csharp",
            "bash",
            "css",
        ] {
            let bundle = LANG_BUNDLES
                .get(lang)
                .unwrap_or_else(|| panic!("no bundle for {lang}"));
            assert!(
                bundle.chunk_query.is_some(),
                "chunk query failed to compile for {lang}"
            );
        }
    }

    #[test]
    fn flush_chunk_insert_replaces_fts_rows_for_same_uid() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_schema(&conn).unwrap();
        let uid = "proj::chunk::sample.rs::1";

        // memories.project_id has a FOREIGN KEY into projects — seed a row.
        conn.execute(
            "INSERT INTO projects(id, name, created_at, updated_at)
             VALUES('proj', 'proj', 0, 0)",
            [],
        )
        .unwrap();

        let mk_pf = |content: &str| ParsedFile {
            rel: "sample.rs".to_string(),
            file_uid: "proj::file::sample.rs".to_string(),
            lang: "rust",
            bytes: 10,
            file_hash: String::new(),
            chunks: vec![PendingChunk {
                uid: uid.to_string(),
                code: content.to_string(),
                file_path: "sample.rs".to_string(),
                start_line: 1,
                end_line: 2,
                language: "rust".to_string(),
                file_uid: "proj::file::sample.rs".to_string(),
            }],
            imports: Vec::new(),
            error: None,
        };

        let pf = mk_pf("OLD SECRET");
        let tx = conn.unchecked_transaction().unwrap();
        flush_chunk_insert(&tx, "proj", &[&pf], 1, 1000).unwrap();
        tx.commit().unwrap();

        let pf = mk_pf("NEW SAFE");
        let tx = conn.unchecked_transaction().unwrap();
        flush_chunk_insert(&tx, "proj", &[&pf], 1, 2000).unwrap();
        tx.commit().unwrap();

        let n_memories: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE uid = ?", [uid], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n_memories, 1, "memories must hold exactly one row per uid");

        let fts_contents: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT content FROM memories_fts WHERE uid = ?")
                .unwrap();
            let rows = stmt
                .query_map([uid], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(
            fts_contents.len(),
            1,
            "stale fts rows for a replaced uid leaked: {fts_contents:?}"
        );
        assert!(
            fts_contents[0].contains("NEW SAFE"),
            "fts row not updated to new content: {}",
            fts_contents[0]
        );
    }

    #[test]
    fn chunk_uids_stay_unique_when_declarations_share_a_start_line() {
        // Minified/bundled JS declares many functions on one physical line.
        // Two declarations starting on line 1 with different end lines must
        // not produce colliding uids (#562) — a duplicate uid aborts the
        // ingest INSERT with a UNIQUE constraint violation.
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bundled.js");
        std::fs::write(&file, "function a() {} function b() {\n  return 2;\n}\n").unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);

        let starts: Vec<_> = pf.chunks.iter().filter(|c| c.start_line == 1).collect();
        assert!(
            starts.len() >= 2,
            "expected two declarations starting on line 1, got {:?}",
            pf.chunks
                .iter()
                .map(|c| (c.start_line, c.end_line))
                .collect::<Vec<_>>()
        );
        let mut uids: Vec<_> = pf.chunks.iter().map(|c| c.uid.as_str()).collect();
        uids.sort_unstable();
        let n = uids.len();
        uids.dedup();
        assert_eq!(uids.len(), n, "duplicate chunk uids: {:?}", uids);
    }
}
