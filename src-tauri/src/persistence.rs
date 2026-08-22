//! Durable coordination between SQLite (the source of truth) and turbovec.
//!
//! A memory mutation and its index-journal entry commit in the same SQLite
//! transaction. Applying the journal is idempotent, so a process crash can
//! only leave replayable work, never an invisible divergence.

use crate::error::{BiError, BiResult};
use crate::state::AppState;
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;

#[derive(Debug, Clone)]
struct PendingMutation {
    id: i64,
    uid: String,
    operation: String,
    content: Option<String>,
}

pub fn queue_index_upsert(
    tx: &Transaction<'_>,
    project_id: &str,
    uid: &str,
    content: &str,
) -> BiResult<()> {
    queue(tx, project_id, uid, "upsert", Some(content))
}

pub fn queue_index_delete(tx: &Transaction<'_>, project_id: &str, uid: &str) -> BiResult<()> {
    queue(tx, project_id, uid, "delete", None)
}

fn queue(
    tx: &Transaction<'_>,
    project_id: &str,
    uid: &str,
    operation: &str,
    content: Option<&str>,
) -> BiResult<()> {
    let content_hash = content.map(hash_text);
    tx.execute(
        "INSERT INTO index_mutations(project_id, memory_uid, operation, content, content_hash, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            project_id,
            uid,
            operation,
            content,
            content_hash,
            chrono::Utc::now().timestamp_millis()
        ],
    )?;
    Ok(())
}

pub fn pending_count(state: &AppState, project_id: &str) -> BiResult<usize> {
    let conn = state.db.conn()?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM index_mutations WHERE project_id = ?1 AND applied_at IS NULL",
        rusqlite::params![project_id],
        |r| r.get::<_, i64>(0),
    )? as usize)
}

impl AppState {
    /// Replay every committed index mutation for one project. Multiple pending
    /// entries for the same uid collapse to the newest operation.
    pub fn replay_index_mutations(&self, project_id: &str) -> BiResult<usize> {
        let lock_path = self
            .data_dir
            .join("indices")
            .join(format!("{project_id}.mutation.lock"));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&lock)?;

        let pending: Vec<PendingMutation> = {
            let conn = self.db.conn()?;
            let mut stmt = conn.prepare(
                "SELECT id, memory_uid, operation, content
                 FROM index_mutations
                 WHERE project_id = ?1 AND applied_at IS NULL
                 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![project_id], |r| {
                Ok(PendingMutation {
                    id: r.get(0)?,
                    uid: r.get(1)?,
                    operation: r.get(2)?,
                    content: r.get(3)?,
                })
            })?;
            rows.filter_map(Result::ok).collect()
        };
        if pending.is_empty() {
            fs2::FileExt::unlock(&lock)?;
            return Ok(0);
        }

        let max_id = pending.last().map(|m| m.id).unwrap_or(0);
        let mut latest: HashMap<String, PendingMutation> = HashMap::new();
        for mutation in pending {
            latest.insert(mutation.uid.clone(), mutation);
        }

        let mut upserts: Vec<(String, String)> = Vec::new();
        let mut deletes: Vec<String> = Vec::new();
        for mutation in latest.into_values() {
            match mutation.operation.as_str() {
                "upsert" => upserts.push((
                    mutation.uid,
                    mutation.content.ok_or_else(|| {
                        BiError::Index("upsert journal entry has no content".into())
                    })?,
                )),
                "delete" => deletes.push(mutation.uid),
                other => return Err(BiError::Index(format!("unknown journal operation {other}"))),
            }
        }

        let index = self.get_or_load_index(project_id)?;
        for uid in &deletes {
            index.remove(uid)?;
        }
        const BATCH: usize = 32;
        for chunk in upserts.chunks(BATCH) {
            let texts: Vec<&str> = chunk.iter().map(|(_, content)| content.as_str()).collect();
            let vectors = self
                .embedder_for_project(project_id)?
                .embed_batch_uncached(&texts)?;
            let items: Vec<(String, Vec<f32>)> = chunk
                .iter()
                .zip(vectors)
                .map(|((uid, _), vector)| (uid.clone(), vector))
                .collect();
            index.add_batch(&items)?;
        }
        index.flush()?;

        let now = chrono::Utc::now().timestamp_millis();
        let digest = index.uid_digest();
        self.db.write(|tx| {
            tx.execute(
                "UPDATE index_mutations SET applied_at = ?1
                 WHERE project_id = ?2 AND applied_at IS NULL AND id <= ?3",
                rusqlite::params![now, project_id, max_id],
            )?;
            tx.execute(
                "INSERT INTO index_state(project_id, last_applied_mutation, content_digest, verified_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id) DO UPDATE SET
                    last_applied_mutation = MAX(last_applied_mutation, excluded.last_applied_mutation),
                    content_digest = excluded.content_digest,
                    verified_at = excluded.verified_at",
                rusqlite::params![project_id, max_id, digest, now],
            )?;
            Ok(())
        })?;
        fs2::FileExt::unlock(&lock)?;
        Ok(upserts.len() + deletes.len())
    }

    pub fn replay_all_index_mutations(&self) -> BiResult<usize> {
        let project_ids: Vec<String> = {
            let conn = self.db.conn()?;
            let mut stmt = conn.prepare("SELECT id FROM projects ORDER BY id")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(Result::ok).collect()
        };
        let mut applied = 0;
        for project_id in project_ids {
            applied += self.replay_index_mutations(&project_id)?;
        }
        Ok(applied)
    }
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}
