//! Multi-project isolation. Each project gets its own turbovec index and a row in
//! the projects table. The "default" project is auto-created on first run.

use crate::db::log_activity;
use crate::error::{BiError, BiResult};
use crate::state::AppState;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub root_path: Option<String>,
    pub bit_width: i64,
    pub dim: i64,
    pub memory_count: i64,
    pub indexed_count: i64,
    pub embed_model: Option<String>,
    pub watch_enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn list(state: &AppState) -> BiResult<Vec<Project>> {
    let conn = state.db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, description, root_path, bit_width, dim, memory_count,
                indexed_count, embed_model, watch_enabled, created_at, updated_at
         FROM projects ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_project)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

pub fn get(state: &AppState, id: &str) -> BiResult<Project> {
    let conn = state.db.conn()?;
    conn.query_row(
        "SELECT id, name, description, root_path, bit_width, dim, memory_count,
                indexed_count, embed_model, watch_enabled, created_at, updated_at
         FROM projects WHERE id = ?1",
        rusqlite::params![id],
        row_to_project,
    )
    .map_err(|_| BiError::NotFound(format!("project {id}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProjectInput {
    pub id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub root_path: Option<String>,
    pub bit_width: Option<u8>,
}

pub fn create(state: &AppState, input: CreateProjectInput) -> BiResult<Project> {
    let id = input.id.unwrap_or_else(|| slugify(&input.name));
    validate_project_id(&id)?;
    let bit_width = input.bit_width.unwrap_or(4);
    if !(2..=4).contains(&bit_width) {
        return Err(BiError::Invalid(format!(
            "bit_width must be one of 2, 3, 4 (got {bit_width})"
        )));
    }
    let bit_width = bit_width as i64;
    let dim = state.embedder.dim as i64;
    let now = chrono::Utc::now().timestamp_millis();

    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO projects(id, name, description, root_path, bit_width, dim, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
            rusqlite::params![
                id,
                input.name,
                input.description,
                input.root_path,
                bit_width,
                dim,
                now
            ],
        )?;
        log_activity(tx, Some(&id), None, "create_project", None, None)?;
        Ok(())
    })?;

    // Write .biTurbo file if root_path is provided and file doesn't exist
    if let Some(ref root_path) = input.root_path {
        let biturbo_file = std::path::PathBuf::from(root_path).join(".biTurbo");
        // Only write if file doesn't exist (skip/continue if it exists)
        if !biturbo_file.exists() {
            let _ = std::fs::write(&biturbo_file, biturbo_file_content(&id, &input.name, now));
        }
    }

    state.refresh_indices()?;
    get(state, &id)
}

/// Contents written to a project root's `.biTurbo` marker file.
/// Simple `key=value` lines; the MCP server only reads the `projectName=`
/// line, so extra fields are safe to add without breaking existing parsers.
fn biturbo_file_content(id: &str, name: &str, created_at: i64) -> String {
    format!("projectId={id}\nprojectName={name}\ncreatedAt={created_at}\nbiturboVersion=1\n")
}

#[derive(Debug, Clone, Serialize)]
pub struct EnsureMarkerFilesResult {
    pub project_id: String,
    pub created: Vec<String>,
}

/// Regenerate `.biTurbo` and/or `.biturboignore` for a project whose root_path
/// predates these marker files (legacy projects). Idempotent — files that
/// already exist are left untouched.
pub fn ensure_marker_files(
    state: &AppState,
    project_id: &str,
) -> BiResult<EnsureMarkerFilesResult> {
    let p = get(state, project_id)?;
    let root_path = p
        .root_path
        .clone()
        .ok_or_else(|| BiError::Invalid(format!("project '{project_id}' has no root_path set")))?;
    let root = std::path::PathBuf::from(&root_path);
    if !root.is_dir() {
        return Err(BiError::Invalid(format!(
            "root_path '{root_path}' does not exist on disk"
        )));
    }

    let mut created = Vec::new();

    let biturbo_file = root.join(".biTurbo");
    if !biturbo_file.exists() {
        let now = chrono::Utc::now().timestamp_millis();
        std::fs::write(&biturbo_file, biturbo_file_content(&p.id, &p.name, now))
            .map_err(|e| BiError::Invalid(format!("failed to write .biTurbo: {e}")))?;
        created.push(".biTurbo".to_string());
    }

    if crate::ingest::ensure_biturboignore(&root)? {
        created.push(".biturboignore".to_string());
    }

    Ok(EnsureMarkerFilesResult {
        project_id: p.id,
        created,
    })
}

pub fn delete(state: &AppState, id: &str) -> BiResult<()> {
    if id == state.default_project_id {
        return Err(BiError::Invalid("cannot delete default project".into()));
    }
    // Defense in depth: a legacy row with a traversal-shaped id must never
    // reach the filesystem path joins below (both the mutation lock file and
    // the index files are named after the raw id).
    validate_project_id(id)?;
    get(state, id)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(
            state
                .data_dir
                .join("indices")
                .join(format!("{id}.mutation.lock")),
        )?;
    fs2::FileExt::lock_exclusive(&lock)?;
    state.flush_all_indices();
    state.indices.write().remove(id);
    // Remove derived state first. If the process stops here, SQLite still
    // contains the project and startup repair deterministically rebuilds it.
    let file = state.data_dir.join("indices").join(format!("{id}.tvim"));
    let meta = file.with_extension("uidmap.json");
    for path in [&file, &meta] {
        if path.exists() {
            std::fs::remove_file(path).map_err(|error| {
                BiError::Io(format!("remove project index {}: {error}", path.display()))
            })?;
        }
    }
    let deleted = state.db.write(|tx| {
        tx.execute(
            "DELETE FROM memories WHERE project_id = ?1",
            rusqlite::params![id],
        )?;
        tx.execute("DELETE FROM projects WHERE id = ?1", rusqlite::params![id])?;
        log_activity(tx, Some(id), None, "delete_project", None, None)?;
        Ok(())
    });
    fs2::FileExt::unlock(&lock)?;
    if deleted.is_err() {
        let _ = state.repair_index_if_needed(id);
    }
    deleted
}

fn row_to_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        root_path: r.get(3)?,
        bit_width: r.get(4)?,
        dim: r.get(5)?,
        memory_count: r.get(6)?,
        indexed_count: r.get(7)?,
        embed_model: r.get(8)?,
        watch_enabled: r.get::<_, i64>(9)? != 0,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// Resolve which project to use for search/recall.
/// Prefers an explicit `project_id`, then `.biTurbo` / `root_path` lookup, else default.
pub fn resolve_project_id(
    state: &AppState,
    project_id: Option<&str>,
    root_path: Option<&str>,
) -> BiResult<String> {
    if let Some(pid) = project_id.filter(|s| !s.is_empty()) {
        return Ok(pid.to_string());
    }
    if let Some(root) = root_path.filter(|s| !s.is_empty()) {
        let biturbo_file = std::path::PathBuf::from(root).join(".biTurbo");
        if biturbo_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&biturbo_file) {
                for line in content.lines() {
                    if let Some(name) = line.strip_prefix("projectName=") {
                        let name = name.trim();
                        if name.is_empty() {
                            continue;
                        }
                        let slug = slugify(name);
                        if get(state, &slug).is_ok() {
                            return Ok(slug);
                        }
                        let conn = state.db.conn()?;
                        let found: Option<String> = conn
                            .query_row(
                                "SELECT id FROM projects WHERE name = ?1 OR id = ?1 LIMIT 1",
                                rusqlite::params![name],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(id) = found {
                            return Ok(id);
                        }
                    }
                }
            }
        }
        if let Ok(canonical) = std::fs::canonicalize(root) {
            let canonical = canonical.to_string_lossy().to_string();
            let conn = state.db.conn()?;
            let found: Option<String> = conn
                .query_row(
                    "SELECT id FROM projects WHERE root_path = ?1 LIMIT 1",
                    rusqlite::params![canonical],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = found {
                return Ok(id);
            }
        }
    }
    Ok(state.default_project_id.clone())
}

pub fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Project ids become filenames (`indices/{id}.tvim`) and reach SQL and the
/// `.biTurbo` marker, so they are restricted to a safe charset. Blocks path
/// traversal (`../..`), separators, whitespace/control chars, and absurd
/// lengths. Slugified names always pass.
pub fn validate_project_id(id: &str) -> BiResult<()> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(BiError::Invalid(format!(
            "project id '{id}' is invalid: use 1-64 characters of [a-zA-Z0-9_-]"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugified_names_always_pass_validation() {
        for name in ["My Project", "äöh_what??", "a/b/c"] {
            validate_project_id(&slugify(name)).expect(name);
        }
        // Names with no slugifiable characters collapse to an empty id and
        // are rejected — create_project surfaces that as Invalid instead of
        // persisting a broken empty-id row.
        assert!(validate_project_id(&slugify("...")).is_err());
    }

    #[test]
    fn traversal_and_separator_ids_are_rejected() {
        for bad in [
            "../../etc/passwd",
            "foo/bar",
            "foo\\bar",
            "..",
            "line1\nline2",
            "",
            "has space",
        ] {
            assert!(validate_project_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn id_length_is_capped() {
        let ok = "a".repeat(64);
        assert!(validate_project_id(&ok).is_ok());
        let too_long = "a".repeat(65);
        assert!(validate_project_id(&too_long).is_err());
    }
}
