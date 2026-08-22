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

/// Create a project-local `.biturboignore` on first ingest.
///
/// The ignore crate already respects `.gitignore`, global gitignore, `.git/info/exclude`,
/// and `.ignore`. `.biturboignore` gives users a biTurbo-specific layer for files that
/// should stay in git but not in semantic memory, such as generated SDKs, fixtures,
/// snapshots, huge examples, or noisy legacy folders.
pub(crate) fn ensure_biturboignore(root: &Path) -> BiResult<bool> {
    let biturboignore = root.join(".biturboignore");
    if biturboignore.exists() {
        return Ok(false);
    }

    let mut content = String::from(
        "# biTurbo ignore file\n\
         # Patterns use gitignore syntax and are applied only by biTurbo ingest.\n\
         # This file was created from .gitignore on first ingest.\n\
         # Add files/folders here that should remain in git but stay out of semantic memory.\n\n",
    );

    let gitignore = root.join(".gitignore");
    if gitignore.exists() {
        let gitignore_content = std::fs::read_to_string(&gitignore)
            .map_err(|e| BiError::Ingest(format!("failed to read {}: {e}", gitignore.display())))?;
        content.push_str("# --- copied from .gitignore ---\n");
        content.push_str(&gitignore_content);
        if !content.ends_with('\n') {
            content.push('\n');
        }
    } else {
        content.push_str("# Examples:\n");
        content.push_str("# dist/\n# target/\n# generated/\n# **/*.snap\n");
    }

    std::fs::write(&biturboignore, content).map_err(|e| {
        BiError::Ingest(format!("failed to write {}: {e}", biturboignore.display()))
    })?;

    Ok(true)
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
    ingest_project_controlled(state, project_id, root, None)
}

pub fn ingest_project_controlled(
    state: &AppState,
    project_id: &str,
    root: &Path,
    operation_id: Option<&str>,
) -> BiResult<IngestResult> {
    if !root.is_dir() {
        return Err(BiError::Ingest(format!("not a dir: {}", root.display())));
    }
    check_cancelled(state, operation_id)?;
    if let Some(id) = operation_id {
        crate::operations::update_progress(state, id, "scanning", 0, 1, None)?;
    }
    let mut result = IngestResult {
        project_id: project_id.to_string(),
        ..Default::default()
    };

    ensure_biturboignore(root)?;

    emit_progress(state, project_id, "scanning", 0, 1, None, 0);

    let files: Vec<PathBuf> = WalkBuilder::new(root)
        .follow_links(false)
        .standard_filters(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .add_custom_ignore_filename(".biturboignore")
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
                if done.is_multiple_of(PROGRESS_EVERY) {
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
    check_cancelled(state, operation_id)?;
    if let Some(id) = operation_id {
        crate::operations::update_progress(state, id, "parsing", files.len(), files.len(), None)?;
    }

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

    // Vector changes are journaled in the SQLite transaction below and replayed
    // only after that source-of-truth commit succeeds.
    let total_chunks = pending_chunks.len();
    for wave in pending_chunks.chunks(CHUNK_INSERT_BATCH) {
        check_cancelled(state, operation_id)?;
        for c in wave {
            let key = format!("{}\0{}\0member_of", c.uid, c.file_uid);
            if edge_keys.insert(key) {
                pending_edges.push((c.uid.clone(), c.file_uid.clone(), "member_of".into(), 1.0));
            }
        }
    }

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
        for uid in &stale_uids {
            crate::persistence::queue_index_delete(tx, project_id, uid)?;
        }
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

    check_cancelled(state, operation_id)?;
    emit_progress(
        state,
        project_id,
        "embedding",
        total_chunks,
        total_chunks,
        None,
        total_chunks,
    );
    if let Some(id) = operation_id {
        crate::operations::update_progress(
            state,
            id,
            "embedding",
            total_chunks,
            total_chunks,
            None,
        )?;
    }
    state.replay_index_mutations(project_id)?;

    state.embedder_for_project(project_id)?.force_release();

    Ok(result)
}

fn check_cancelled(state: &AppState, operation_id: Option<&str>) -> BiResult<()> {
    if let Some(id) = operation_id {
        if crate::operations::is_cancel_requested(state, id)? {
            return Err(BiError::Ingest("operation cancelled".into()));
        }
    }
    Ok(())
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
        // Skip the leading `// path:start-end` comment line when classifying —
        // kind detection and labels must look at the actual code, not the path hint.
        "SELECT uid,
                CASE
                  WHEN content LIKE '// %' AND instr(content, char(10)) > 0
                    THEN substr(content, instr(content, char(10)) + 1)
                  ELSE content
                END AS code_hint,
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
            // Classify from the first code line (not the path comment). Prefer
            // declaration keywords so method bodies that mention "class" later
            // still count as functions.
            let kind = classify_code_kind(content_hint);
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

fn classify_code_kind(content_hint: &str) -> &'static str {
    let first = content_hint
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty() && !l.starts_with("//") && !l.starts_with('#') && !l.starts_with("/*")
        })
        .unwrap_or(content_hint.trim());
    let lower = first.to_ascii_lowercase();
    // Kotlin/Java/TS/Python class-like declarations.
    if lower.contains("class ")
        || lower.starts_with("class")
        || lower.contains("interface ")
        || lower.contains("object ")
        || lower.contains("enum class")
        || lower.contains("enum ")
    {
        "class"
    } else if lower.contains("struct ") || lower.starts_with("struct") {
        "struct"
    } else {
        "function"
    }
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
    for pf in batch {
        for c in &pf.chunks {
            crate::persistence::queue_index_upsert(tx, project_id, &c.uid, &c.embed_text())?;
        }
    }
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
        "sql" => "sql",
        "dart" => "dart",
        "lua" => "lua",
        "scala" | "sc" => "scala",
        "r" | "R" => "r",
        "ps1" | "psm1" => "powershell",
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
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "bash" => tree_sitter_bash::LANGUAGE.into(),
        "html" => tree_sitter_html::LANGUAGE.into(),
        "css" => tree_sitter_css::LANGUAGE.into(),
        "sql" => tree_sitter_sequel::LANGUAGE.into(),
        "dart" => tree_sitter_dart_orchard::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "r" => tree_sitter_r::LANGUAGE.into(),
        "powershell" => tree_sitter_powershell::LANGUAGE.into(),
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

const SUPPORTED_LANGUAGES: &[&str] = &[
    "rust",
    "typescript",
    "javascript",
    "python",
    "go",
    "swift",
    "php",
    "ruby",
    "java",
    "kotlin",
    "c",
    "cpp",
    "csharp",
    "bash",
    "html",
    "css",
    "sql",
    "dart",
    "lua",
    "scala",
    "r",
    "powershell",
];

static LANG_BUNDLES: Lazy<HashMap<&'static str, LangBundle>> = Lazy::new(|| {
    let mut map = HashMap::new();
    for &name in SUPPORTED_LANGUAGES {
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
        "kotlin" => {
            r#"
            (function_declaration) @def
            (class_declaration) @def
            (object_declaration) @def
            (companion_object) @def
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
        "sql" => {
            r#"
            (create_table) @def
            (create_view) @def
            (create_materialized_view) @def
            (create_function) @def
            (create_trigger) @def
            (create_type) @def
        "#
        }
        "dart" => {
            r#"
            (class_definition) @def
            (enum_declaration) @def
            (mixin_declaration) @def
            (extension_declaration) @def
            (function_signature) @def
            (local_function_declaration) @def
        "#
        }
        "lua" => {
            r#"
            (function_declaration) @def
            (function_definition) @def
        "#
        }
        "scala" => {
            r#"
            (class_definition) @def
            (object_definition) @def
            (trait_definition) @def
            (enum_definition) @def
            (function_definition) @def
        "#
        }
        "r" => {
            r#"
            (binary_operator
              lhs: (identifier)
              operator: ["<-" "<<-" "="]
              rhs: (function_definition)) @def
        "#
        }
        "powershell" => {
            r#"
            (function_statement) @def
            (class_statement) @def
            (enum_statement) @def
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
                if query.capture_names()[cap.index as usize] != "def" {
                    continue;
                }
                let node = cap.node;
                let end_node = if node.kind() == "function_signature" {
                    node.next_named_sibling()
                        .filter(|sibling| sibling.kind() == "function_body")
                        .unwrap_or(node)
                } else {
                    node
                };
                let start = node.start_position().row;
                let end = end_node
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
        "kotlin" => {
            r#"
            (import_header) @imp
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
        "dart" => {
            r#"
            (library_import
              (import_specification
                (configurable_uri
                  (uri (string_literal) @imp))))
            (library_export
              (configurable_uri
                (uri (string_literal) @imp)))
        "#
        }
        "lua" => {
            r#"
            (function_call
              name: (identifier) @fn
              arguments: (arguments (string) @imp)
              (#eq? @fn "require"))
        "#
        }
        "scala" => {
            r#"
            (import_declaration) @imp
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
            if query.capture_names()[cap.index as usize] != "imp" {
                continue;
            }
            let node = cap.node;
            if matches!(
                node.kind(),
                "string" | "string_literal" | "interpreted_string_literal"
            ) {
                let raw = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                let cleaned = raw.trim_matches(|c| c == '"' || c == '\'').to_string();
                if !cleaned.is_empty() {
                    imports.push(cleaned);
                }
            } else if matches!(node.kind(), "import_declaration" | "import_header") {
                let raw = node.utf8_text(source.as_bytes()).unwrap_or("");
                let cleaned = raw
                    .strip_prefix("import")
                    .unwrap_or(raw)
                    .trim()
                    .trim_end_matches(';')
                    .to_string();
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

    if import.contains('.') && !import.contains(':') {
        let candidate = root.join(import.replace('.', "/"));
        for c in expand_candidates(&candidate) {
            if c.exists() {
                let rel = c.strip_prefix(root).ok()?.to_string_lossy().to_string();
                return Some(rel);
            }
        }
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
    let exts = [
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "dart", "lua", "scala", "sc",
        "r", "R", "ps1", "psm1",
    ];
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
    fn parse_one_file_extracts_sql_definitions() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("schema.sql");
        std::fs::write(
            &file,
            "CREATE TABLE users (id INTEGER PRIMARY KEY);\n\nCREATE VIEW active_users AS SELECT * FROM users;\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "sql");
        assert!(
            pf.chunks.len() >= 2,
            "chunks: {:?}",
            pf.chunks
                .iter()
                .map(PendingChunk::db_content)
                .collect::<Vec<_>>()
        );
        assert!(pf.chunks.iter().any(|c| c.db_content().contains("users")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_one_file_extracts_dart_definitions_and_imports() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("widget.dart");
        std::fs::write(
            &file,
            "import 'package:flutter/widgets.dart';\n\nvoid main() {\n  runApp(CounterWidget());\n}\n\nclass CounterWidget {\n  int value() => 1;\n}\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "dart");
        assert!(pf
            .chunks
            .iter()
            .any(|c| c.db_content().contains("CounterWidget")));
        assert!(pf.chunks.iter().any(|c| {
            let content = c.db_content();
            content.contains("void main()") && content.contains("runApp")
        }));
        assert_eq!(pf.imports, vec!["package:flutter/widgets.dart"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_one_file_extracts_lua_definitions_and_requires() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("player.lua");
        std::fs::write(
            &file,
            "local physics = require('game.physics')\n\nfunction spawn_player()\n  return physics.spawn()\nend\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "lua");
        assert!(pf
            .chunks
            .iter()
            .any(|c| c.db_content().contains("spawn_player")));
        assert_eq!(pf.imports, vec!["game.physics"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_one_file_extracts_scala_definitions_and_imports() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Service.scala");
        std::fs::write(
            &file,
            "import scala.concurrent.Future\n\nclass UserService {\n  def load(): Future[String] = Future.successful(\"ok\")\n}\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "scala");
        assert!(pf
            .chunks
            .iter()
            .any(|c| c.db_content().contains("UserService")));
        assert_eq!(pf.imports, vec!["scala.concurrent.Future"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_one_file_extracts_named_r_functions() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("analysis.R");
        std::fs::write(
            &file,
            "summarise_data <- function(values) {\n  mean(values)\n}\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "r");
        assert!(pf
            .chunks
            .iter()
            .any(|c| c.db_content().contains("summarise_data")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_one_file_extracts_powershell_definitions() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Deploy.ps1");
        std::fs::write(
            &file,
            "function Invoke-Deploy {\n  param([string]$Environment)\n  Write-Output $Environment\n}\n",
        )
        .unwrap();

        let pf = parse_one_file("proj", &dir, &file, &HashMap::new());
        assert!(pf.error.is_none(), "error: {:?}", pf.error);
        assert_eq!(pf.lang, "powershell");
        assert!(pf
            .chunks
            .iter()
            .any(|c| c.db_content().contains("Invoke-Deploy")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_import_maps_dotted_modules_to_new_language_files() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-ingest-test-{}", uuid::Uuid::new_v4()));
        let lua_module = dir.join("game/physics.lua");
        let scala_module = dir.join("app/models/User.scala");
        std::fs::create_dir_all(lua_module.parent().unwrap()).unwrap();
        std::fs::create_dir_all(scala_module.parent().unwrap()).unwrap();
        std::fs::write(&lua_module, "return {}\n").unwrap();
        std::fs::write(&scala_module, "class User\n").unwrap();

        assert_eq!(
            resolve_import(
                "game.physics",
                &dir.join("main.lua"),
                &dir,
                &BTreeMap::new()
            ),
            Some("game/physics.lua".to_string())
        );
        assert_eq!(
            resolve_import(
                "app.models.User",
                &dir.join("Main.scala"),
                &dir,
                &BTreeMap::new()
            ),
            Some("app/models/User.scala".to_string())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lang_bundles_compile_for_all_languages() {
        for &lang in SUPPORTED_LANGUAGES {
            let bundle = LANG_BUNDLES
                .get(lang)
                .unwrap_or_else(|| panic!("no bundle for {lang}"));
            if lang != "html" {
                assert!(
                    bundle.chunk_query.is_some(),
                    "chunk query failed to compile for {lang}"
                );
            }
        }
    }

    #[test]
    fn flush_chunk_insert_replaces_fts_rows_for_same_uid() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&mut conn).unwrap();
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
