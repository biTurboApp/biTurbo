//! SQLite schema + connection pool. Holds all metadata; turbovec holds vectors.
//!
//! Tables
//! ──────
//! projects  — multi-project isolation (one index per project + a global one)
//! memories  — memory entries with type, importance, tags, access stats
//! agents    — registered AI agents (Mavis, Cursor, Claude Code, Cline…)
//! activity  — append-only audit log of writes/reads
//! code_index — tree-sitter chunks per project (file:range → memory)

use crate::error::BiResult;
use parking_lot::Mutex;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub type DbPool = Pool<SqliteConnectionManager>;

pub fn open_pool(db_path: &Path) -> BiResult<DbPool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let manager = SqliteConnectionManager::file(db_path).with_init(|c| {
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-16384;          -- 16 MiB page cache per connection
             PRAGMA mmap_size=67108864;         -- 64 MiB memory-mapped reads
             PRAGMA wal_autocheckpoint=2000;",
        )?;
        c.set_prepared_statement_cache_capacity(64);
        Ok(())
    });
    let pool = Pool::builder().max_size(6).build(manager)?;
    let conn = pool.get()?;
    init_schema(&conn)?;
    Ok(pool)
}

pub fn rebuild_fts_index(conn: &rusqlite::Connection) -> BiResult<usize> {
    conn.execute_batch(
        "DELETE FROM memories_fts;
         INSERT INTO memories_fts(uid, content, tags, mem_type, project_id)
         SELECT uid, content, COALESCE(tags, ''), mem_type, project_id FROM memories;",
    )?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM memories_fts", [], |r| r.get(0))?;
    Ok(n as usize)
}

pub fn init_schema(conn: &rusqlite::Connection) -> BiResult<()> {
    conn.execute_batch(SCHEMA)?;

    let mut added_decay_base = false;
    for (table, col, decl) in &[
        ("projects", "embed_model", "TEXT"),
        ("projects", "watch_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ("memories", "decay_base", "REAL"),
    ] {
        let present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, col],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if present == 0 {
            let sql = format!("ALTER TABLE {table} ADD COLUMN {col} {decl}");
            conn.execute_batch(&sql)?;
            if *col == "decay_base" {
                added_decay_base = true;
            }
        }
    }

    // Seed the decay baseline from current importances exactly once, so
    // consolidate can recompute decayed importance idempotently (#435).
    if added_decay_base {
        conn.execute("UPDATE memories SET decay_base = importance", [])?;
    }

    // One-time repair for FTS rows left stale by INSERT OR REPLACE bypassing
    // the delete trigger (#525): rebuild the full-text index once per database.
    let fts_repaired: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'fts_rebuild_v1'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if fts_repaired.is_none() {
        let n = rebuild_fts_index(conn)?;
        conn.execute(
            "INSERT INTO settings(key, value) VALUES('fts_rebuild_v1', '1')",
            [],
        )?;
        tracing::info!("one-time FTS rebuild after upgrade: {n} rows reindexed");
    }

    Ok(())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    description   TEXT,
    root_path     TEXT,
    bit_width     INTEGER NOT NULL DEFAULT 4,
    dim           INTEGER NOT NULL DEFAULT 384,
    memory_count  INTEGER NOT NULL DEFAULT 0,
    indexed_count INTEGER NOT NULL DEFAULT 0,
    embed_model   TEXT,
    watch_enabled INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    kind        TEXT NOT NULL,
    last_seen   INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    meta        TEXT
);

CREATE TABLE IF NOT EXISTS memories (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    uid          TEXT UNIQUE NOT NULL,
    project_id   TEXT NOT NULL,
    mem_type     TEXT NOT NULL,        -- fact | decision | preference | pattern | episode | reflection | code
    content      TEXT NOT NULL,
    tags         TEXT,                 -- JSON array
    source_agent TEXT,
    importance   REAL NOT NULL DEFAULT 0.5,
    supersedes   INTEGER,              -- memory id this one replaces
    superseded_by INTEGER,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    last_access  INTEGER NOT NULL,
    access_count INTEGER NOT NULL DEFAULT 0,
    file_path    TEXT,                 -- for code_index entries
    start_line   INTEGER,
    end_line     INTEGER,
    language     TEXT,
    FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);
CREATE INDEX IF NOT EXISTS idx_memories_type    ON memories(mem_type);
CREATE INDEX IF NOT EXISTS idx_memories_imp     ON memories(importance DESC);
CREATE INDEX IF NOT EXISTS idx_memories_time    ON memories(created_at DESC);
-- Covers the common list/search filter (project + type, newest first) without a sort pass.
CREATE INDEX IF NOT EXISTS idx_memories_proj_type_time ON memories(project_id, mem_type, created_at DESC);

CREATE TABLE IF NOT EXISTS activity (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT,
    agent_id    TEXT,
    action      TEXT NOT NULL,         -- write | read | forget | update | search | consolidate | ingest
    memory_uid  TEXT,
    detail      TEXT,                  -- JSON
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_activity_time ON activity(created_at DESC);

CREATE TABLE IF NOT EXISTS code_edges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT NOT NULL,
    from_uid    TEXT NOT NULL,
    to_uid      TEXT NOT NULL,
    edge_type   TEXT NOT NULL,
    weight      REAL NOT NULL DEFAULT 1.0,
    created_at  INTEGER NOT NULL,
    FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_code_edges_project ON code_edges(project_id);
CREATE INDEX IF NOT EXISTS idx_code_edges_from ON code_edges(from_uid);
CREATE INDEX IF NOT EXISTS idx_code_edges_to ON code_edges(to_uid);

CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
    uid UNINDEXED,
    content,
    tags,
    mem_type UNINDEXED,
    project_id UNINDEXED,
    tokenize='porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
    INSERT INTO memories_fts(uid, content, tags, mem_type, project_id)
    VALUES (new.uid, new.content, COALESCE(new.tags, ''), new.mem_type, new.project_id);
END;
CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
    DELETE FROM memories_fts WHERE uid = old.uid;
END;
CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
    UPDATE memories_fts SET
        content = new.content,
        tags = COALESCE(new.tags, ''),
        mem_type = new.mem_type
    WHERE uid = old.uid;
END;

CREATE TABLE IF NOT EXISTS indexed_files (
    project_id  TEXT NOT NULL,
    file_path   TEXT NOT NULL,
    file_hash   TEXT NOT NULL,
    language    TEXT NOT NULL,
    imports_json TEXT NOT NULL DEFAULT '[]',
    indexed_at  INTEGER NOT NULL,
    PRIMARY KEY(project_id, file_path),
    FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_indexed_files_project ON indexed_files(project_id);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub struct Db {
    pub pool: Arc<DbPool>,
    pub write_lock: Arc<Mutex<()>>,
}

impl Clone for Db {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            write_lock: self.write_lock.clone(),
        }
    }
}

impl Db {
    pub fn open(db_path: &Path) -> BiResult<Self> {
        let pool = open_pool(db_path)?;
        Ok(Self {
            pool: Arc::new(pool),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn conn(&self) -> BiResult<r2d2::PooledConnection<SqliteConnectionManager>> {
        Ok(self.pool.get()?)
    }

    /// Run a write transaction with an exclusive lock so we never race with the MCP server.
    pub fn write<F, T>(&self, f: F) -> BiResult<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> BiResult<T>,
    {
        let _g = self.write_lock.lock();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

pub fn get_setting(conn: &rusqlite::Connection, key: &str) -> BiResult<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v)
}

pub fn set_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> BiResult<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct IndexedFileInfo {
    pub file_hash: String,
    pub language: String,
    pub imports: Vec<String>,
}

pub fn get_indexed_files(
    conn: &rusqlite::Connection,
    project_id: &str,
) -> BiResult<HashMap<String, IndexedFileInfo>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, file_hash, language, imports_json
         FROM indexed_files
         WHERE project_id = ?1",
    )?;
    let rows = stmt.query_map(params![project_id], |r| {
        let imports_json: Option<String> = r.get(3)?;
        let imports = imports_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        Ok((
            r.get::<_, String>(0)?,
            IndexedFileInfo {
                file_hash: r.get::<_, String>(1)?,
                language: r.get::<_, String>(2)?,
                imports,
            },
        ))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn upsert_indexed_file(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_path: &str,
    file_hash: &str,
    language: &str,
    imports: &[String],
    now: i64,
) -> BiResult<()> {
    let imports_json = serde_json::to_string(imports)?;
    tx.execute(
        "INSERT INTO indexed_files(project_id, file_path, file_hash, language, imports_json, indexed_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(project_id, file_path) DO UPDATE SET
            file_hash = excluded.file_hash,
            language = excluded.language,
            imports_json = excluded.imports_json,
            indexed_at = excluded.indexed_at",
        params![project_id, file_path, file_hash, language, imports_json, now],
    )?;
    Ok(())
}

pub fn delete_indexed_file(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_path: &str,
) -> BiResult<()> {
    tx.execute(
        "DELETE FROM indexed_files WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;
    Ok(())
}

pub fn code_uids_for_file(
    conn: &rusqlite::Connection,
    project_id: &str,
    file_path: &str,
) -> BiResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT uid FROM memories
         WHERE project_id = ?1 AND mem_type = 'code' AND file_path = ?2
         ORDER BY start_line",
    )?;
    let rows = stmt.query_map(params![project_id, file_path], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn delete_memories_by_uids(tx: &rusqlite::Transaction<'_>, uids: &[String]) -> BiResult<()> {
    for chunk in uids.chunks(400) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM memories WHERE uid IN ({placeholders})");
        let mut stmt = tx.prepare(&sql)?;
        stmt.execute(rusqlite::params_from_iter(
            chunk.iter().map(|uid| uid.as_str()),
        ))?;
    }
    Ok(())
}

pub fn delete_code_edges_for_files(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_uids: &[String],
) -> BiResult<()> {
    for chunk in file_uids.chunks(400) {
        let placeholders = std::iter::repeat("?")
            .take(chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM code_edges
             WHERE project_id = ?1 AND (from_uid IN ({placeholders}) OR to_uid IN ({placeholders}))"
        );
        let mut stmt = tx.prepare(&sql)?;
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(1 + chunk.len() * 2);
        params.push(project_id.to_string().into());
        params.extend(chunk.iter().cloned().map(rusqlite::types::Value::Text));
        params.extend(chunk.iter().cloned().map(rusqlite::types::Value::Text));
        stmt.execute(rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

pub fn log_activity(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
    agent_id: Option<&str>,
    action: &str,
    memory_uid: Option<&str>,
    detail: Option<&serde_json::Value>,
) -> BiResult<()> {
    let detail_str = detail.map(|v| v.to_string());
    let mut stmt = conn.prepare_cached(
        "INSERT INTO activity(project_id, agent_id, action, memory_uid, detail, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    stmt.execute(params![
        project_id,
        agent_id,
        action,
        memory_uid,
        detail_str,
        chrono::Utc::now().timestamp_millis(),
    ])?;
    Ok(())
}
