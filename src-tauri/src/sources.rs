//! Read-only local transcript adapters for Codex and Claude Code JSONL data.

use crate::capture::{ObservationSource, SubmitObservationInput};
use crate::error::{BiError, BiResult};
use crate::state::AppState;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Seek, SeekFrom};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AdapterCursor {
    version: u8,
    files: BTreeMap<String, FileCursor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FileCursor {
    offset: u64,
    line: usize,
    prefix_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSyncResult {
    pub source_id: String,
    pub files_scanned: usize,
    pub observations_processed: usize,
    pub candidates_created: usize,
    pub duplicates: usize,
    pub warnings: Vec<String>,
}

pub fn get_source(state: &AppState, source_id: &str) -> BiResult<ObservationSource> {
    crate::capture::list_sources(state, None)?
        .into_iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| BiError::NotFound(format!("observation source {source_id}")))
}

pub fn sync_source(
    state: &AppState,
    source_id: &str,
    operation_id: Option<&str>,
) -> BiResult<SourceSyncResult> {
    let source = get_source(state, source_id)?;
    if !source.enabled {
        return Err(BiError::Invalid(format!("source {source_id} is disabled")));
    }
    if !matches!(source.kind.as_str(), "codex" | "claude_code") {
        return Err(BiError::Invalid(
            "generic observations are submitted directly and cannot be synced".into(),
        ));
    }
    let root = source
        .root_path
        .as_deref()
        .ok_or_else(|| BiError::Invalid("source root_path is missing".into()))?;
    let root = std::path::Path::new(root);
    if !root.is_dir() {
        return Err(BiError::Invalid(format!(
            "source directory '{}' is unavailable",
            root.display()
        )));
    }
    let mut files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .collect();
    files.sort();
    let mut result = SourceSyncResult {
        source_id: source.id.clone(),
        files_scanned: files.len(),
        observations_processed: 0,
        candidates_created: 0,
        duplicates: 0,
        warnings: Vec::new(),
    };
    let mut cursor = load_adapter_cursor(state, &source.id)?;
    cursor.version = 1;
    let total_bytes: u64 = files
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .sum();
    let mut consumed = 0u64;
    for path in &files {
        if operation_id
            .is_some_and(|id| crate::operations::is_cancel_requested(state, id).unwrap_or(false))
        {
            return Err(BiError::Invalid("source sync cancelled".into()));
        }
        let relative = path.strip_prefix(root).unwrap_or(path);
        let relative_key = relative.to_string_lossy().to_string();
        let prior = cursor.files.get(&relative_key).cloned().unwrap_or_default();
        let (start, start_line, mut prefix_hasher) = resume_position(path, &prior)?;
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut reader = std::io::BufReader::new(file);
        let mut offset = start;
        let mut line_number = start_line;
        consumed = consumed.saturating_add(start);
        loop {
            let mut line = String::new();
            let bytes = match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(bytes) => bytes,
                Err(error) => {
                    result.warnings.push(format!(
                        "{}:{}: {error}",
                        relative.display(),
                        line_number + 1
                    ));
                    break;
                }
            };
            offset += bytes as u64;
            line_number += 1;
            consumed = consumed.saturating_add(bytes as u64);
            prefix_hasher.update(line.as_bytes());
            cursor.files.insert(
                relative_key.clone(),
                FileCursor {
                    offset,
                    line: line_number,
                    prefix_hash: hex::encode(prefix_hasher.clone().finalize()),
                },
            );
            let line = line.trim_end_matches(['\r', '\n']);
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Some(content) = extract_content(&value) else {
                continue;
            };
            if content.trim().is_empty() {
                continue;
            }
            let event_id = extract_string(&value, &["external_id", "event_id", "eventId", "id"])
                .unwrap_or_else(|| hash_text(&value.to_string()));
            let external_id = format!("{}:{}:{}", source.kind, relative_key, event_id);
            let checkpoint_value = serde_json::to_value(&cursor)?;
            let checkpoint_identity = hash_text(&checkpoint_value.to_string());
            let sync = crate::capture::submit_observation(
                state,
                SubmitObservationInput {
                    project_id: source.project_id.clone(),
                    source_kind: source.kind.clone(),
                    external_id,
                    session_id: extract_string(
                        &value,
                        &["session_id", "sessionId", "conversation_id"],
                    ),
                    occurred_at: extract_timestamp(&value),
                    role: extract_string(&value, &["role", "type"])
                        .unwrap_or_else(|| "unknown".into()),
                    content,
                    metadata: Some(serde_json::json!({
                        "source_id": source.id,
                        "source_pointer": format!("{}:{}", path.display(), line_number),
                        "source_checkpoint": checkpoint_value,
                        "source_checkpoint_identity": checkpoint_identity
                    })),
                },
            )?;
            if sync.duplicate {
                result.duplicates += 1;
            } else {
                result.observations_processed += 1;
                result.candidates_created += sync.candidate_ids.len();
            }
            result.warnings.extend(sync.warnings);
            if let Some(operation_id) = operation_id {
                crate::operations::update_progress(
                    state,
                    operation_id,
                    "reading_transcripts",
                    consumed.min(total_bytes) as usize,
                    total_bytes.max(1) as usize,
                    Some(&serde_json::json!({
                        "source_id": source.id,
                        "file": relative,
                        "line": line_number
                    })),
                )?;
            }
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    cursor
        .files
        .retain(|relative, _| root.join(relative).is_file());
    let checkpoint = serde_json::to_string(&cursor)?;
    let identity = hash_text(&checkpoint);
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO source_checkpoints(source_id, cursor, content_identity, updated_at)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(source_id) DO UPDATE SET cursor=excluded.cursor,
                 content_identity=excluded.content_identity, updated_at=excluded.updated_at",
            rusqlite::params![source.id, checkpoint, identity, now],
        )?;
        tx.execute(
            "UPDATE observation_sources SET last_sync_at=?1, last_error=NULL,
                 updated_at=?1 WHERE id=?2",
            rusqlite::params![now, source.id],
        )?;
        Ok(())
    })?;
    Ok(result)
}

pub fn mark_source_error(state: &AppState, source_id: &str, error: &str) -> BiResult<()> {
    state.db.write(|tx| {
        tx.execute(
            "UPDATE observation_sources SET last_error=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![error, chrono::Utc::now().timestamp_millis(), source_id],
        )?;
        Ok(())
    })
}

pub fn checkpoint(state: &AppState, source_id: &str) -> BiResult<Option<serde_json::Value>> {
    let conn = state.db.conn()?;
    let value: Option<(String, Option<String>, i64)> = conn
        .query_row(
            "SELECT cursor, content_identity, updated_at FROM source_checkpoints WHERE source_id=?1",
            rusqlite::params![source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(value.map(|(cursor, identity, updated_at)| {
        serde_json::json!({"cursor": cursor, "content_identity": identity, "updated_at": updated_at})
    }))
}

fn extract_content(value: &serde_json::Value) -> Option<String> {
    for pointer in [
        "/content",
        "/text",
        "/message/content",
        "/payload/content",
        "/payload/message/content",
        "/event/message/content",
    ] {
        if let Some(content) = value.pointer(pointer).and_then(content_value) {
            return Some(content);
        }
    }
    None
}

fn content_value(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(String::from)
                    .or_else(|| {
                        item.get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(String::from)
                    })
                    .or_else(|| {
                        item.get("content")
                            .and_then(serde_json::Value::as_str)
                            .map(String::from)
                    })
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn extract_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        for pointer in [
            format!("/{key}"),
            format!("/message/{key}"),
            format!("/payload/{key}"),
        ] {
            if let Some(value) = value.pointer(&pointer).and_then(serde_json::Value::as_str) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn extract_timestamp(value: &serde_json::Value) -> i64 {
    for key in ["timestamp", "created_at", "createdAt", "time"] {
        if let Some(number) = value.get(key).and_then(serde_json::Value::as_i64) {
            return if number < 10_000_000_000 {
                number * 1_000
            } else {
                number
            };
        }
        if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
                return parsed.timestamp_millis();
            }
        }
    }
    chrono::Utc::now().timestamp_millis()
}

fn load_adapter_cursor(state: &AppState, source_id: &str) -> BiResult<AdapterCursor> {
    let conn = state.db.conn()?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT cursor FROM source_checkpoints WHERE source_id=?1",
            rusqlite::params![source_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default())
}

fn resume_position(path: &std::path::Path, prior: &FileCursor) -> BiResult<(u64, usize, Sha256)> {
    let mut file = std::fs::File::open(path)?;
    if prior.offset == 0 || file.metadata()?.len() < prior.offset {
        return Ok((0, 0, Sha256::new()));
    }
    let mut hasher = Sha256::new();
    let mut remaining = prior.offset;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let read_len = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..read_len])?;
        if count == 0 {
            return Ok((0, 0, Sha256::new()));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let actual = hex::encode(hasher.clone().finalize());
    if actual != prior.prefix_hash {
        return Ok((0, 0, Sha256::new()));
    }
    Ok((prior.offset, prior.line, hasher))
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_adapter_is_read_only_and_idempotent() {
        let root =
            std::env::temp_dir().join(format!("biturbo-source-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let transcript = root.join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"role\":\"user\",\"content\":\"We decided to keep all memory processing local.\"}\n",
        )
        .unwrap();
        let data =
            std::env::temp_dir().join(format!("biturbo-source-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&data).unwrap();
        let mut policy = crate::capture::get_capture_policy(&state, "default").unwrap();
        policy.enabled_sources.push("codex".into());
        crate::capture::update_capture_policy(&state, policy).unwrap();
        let source = crate::capture::upsert_source(
            &state,
            ObservationSource {
                id: String::new(),
                project_id: "default".into(),
                kind: "codex".into(),
                name: "fixture".into(),
                root_path: Some(root.to_string_lossy().into()),
                enabled: true,
                config: serde_json::json!({}),
                last_sync_at: None,
                last_error: None,
                processed_count: 0,
                candidate_count: 0,
                created_at: 0,
                updated_at: 0,
            },
        )
        .unwrap();
        let first = sync_source(&state, &source.id, None).unwrap();
        let second = sync_source(&state, &source.id, None).unwrap();
        assert_eq!(first.observations_processed, 1);
        assert_eq!(second.observations_processed, 0);
        assert_eq!(second.duplicates, 0);
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript)
                .unwrap(),
            "{{\"id\":\"second\",\"role\":\"user\",\"content\":\"I prefer deterministic local extraction.\"}}"
        )
        .unwrap();
        let appended = sync_source(&state, &source.id, None).unwrap();
        assert_eq!(appended.observations_processed, 1);

        std::fs::write(
            &transcript,
            "{\"role\":\"user\",\"content\":\"We decided to keep all memory processing local.\"}\n{\"id\":\"replacement\",\"role\":\"user\",\"content\":\"The project uses an opaque source checkpoint.\"}\n",
        )
        .unwrap();
        let replaced = sync_source(&state, &source.id, None).unwrap();
        assert_eq!(replaced.observations_processed, 1);
        assert_eq!(replaced.duplicates, 1);
        assert!(std::fs::read_to_string(transcript)
            .unwrap()
            .contains("opaque source checkpoint"));
    }
}
