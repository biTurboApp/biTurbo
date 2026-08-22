//! Transport-neutral application interface shared by Tauri and MCP adapters.

use crate::error::BiResult;
use crate::memory;
use crate::project::{self, Project};
use crate::state::AppState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub total_memories: i64,
    pub total_projects: i64,
    pub total_agents: i64,
    pub by_type: Vec<(String, i64)>,
    pub by_project: Vec<(String, i64)>,
    pub index_bytes: u64,
    pub recent_writes_7d: i64,
    pub recent_reads_7d: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub action: String,
    pub memory_uid: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub last_seen: i64,
    pub created_at: i64,
    pub meta: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub stats: Stats,
    pub projects: Vec<Project>,
    pub recent: Vec<ActivityEntry>,
    pub tags: Vec<(String, i64)>,
    pub agents: Vec<AgentEntry>,
    pub consolidate: crate::scheduler::ConsolidateStatus,
}

pub fn stats(state: &AppState) -> BiResult<Stats> {
    let conn = state.db.conn()?;
    let total_memories = conn.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
    let total_projects = conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))?;
    let total_agents = conn
        .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))
        .unwrap_or(0);
    let by_type = memory::count_by_type(state, None)?;
    let mut by_project: Vec<(String, i64)> = {
        let mut stmt = conn.prepare("SELECT id, memory_count FROM projects")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.filter_map(Result::ok).collect()
    };
    by_project.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let week_ago = chrono::Utc::now().timestamp_millis() - 7 * 24 * 3600 * 1000;
    let recent_writes_7d = conn.query_row(
        "SELECT COUNT(*) FROM activity WHERE action IN ('write','update') AND created_at > ?1",
        rusqlite::params![week_ago],
        |r| r.get(0),
    )?;
    let recent_reads_7d = conn.query_row(
        "SELECT COUNT(*) FROM activity WHERE action = 'read' AND created_at > ?1",
        rusqlite::params![week_ago],
        |r| r.get(0),
    )?;
    Ok(Stats {
        total_memories,
        total_projects,
        total_agents,
        by_type,
        by_project,
        index_bytes: state.index_bytes(),
        recent_writes_7d,
        recent_reads_7d,
    })
}

pub fn recent_activity(state: &AppState, limit: usize) -> BiResult<Vec<ActivityEntry>> {
    let conn = state.db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, project_id, agent_id, action, memory_uid, detail, created_at
         FROM activity ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit.clamp(1, 500) as i64], |r| {
        let detail: Option<String> = r.get(5)?;
        Ok(ActivityEntry {
            id: r.get(0)?,
            project_id: r.get(1)?,
            agent_id: r.get(2)?,
            action: r.get(3)?,
            memory_uid: r.get(4)?,
            detail: detail.and_then(|value| serde_json::from_str(&value).ok()),
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn list_agents(state: &AppState) -> BiResult<Vec<AgentEntry>> {
    let conn = state.db.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, last_seen, created_at, meta FROM agents ORDER BY last_seen DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let meta: Option<String> = r.get(5)?;
        Ok(AgentEntry {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            last_seen: r.get(3)?,
            created_at: r.get(4)?,
            meta: meta.and_then(|value| serde_json::from_str(&value).ok()),
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn register_agent(
    state: &AppState,
    name: String,
    kind: String,
    meta: Option<serde_json::Value>,
) -> BiResult<AgentEntry> {
    let now = chrono::Utc::now().timestamp_millis();
    let id = slugify(&name);
    let meta_json = meta.as_ref().map(serde_json::Value::to_string);
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO agents(id, name, kind, last_seen, created_at, meta)
             VALUES(?1,?2,?3,?4,?4,?5)
             ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen,
                kind = excluded.kind, meta = COALESCE(excluded.meta, agents.meta)",
            rusqlite::params![id, name, kind, now, meta_json],
        )?;
        Ok(())
    })?;
    Ok(AgentEntry {
        id,
        name,
        kind,
        last_seen: now,
        created_at: now,
        meta,
    })
}

pub fn bootstrap(state: &AppState) -> BiResult<Bootstrap> {
    Ok(Bootstrap {
        stats: stats(state)?,
        projects: project::list(state)?,
        recent: recent_activity(state, 25)?,
        tags: memory::list_tags(state, None)?,
        agents: list_agents(state)?,
        consolidate: crate::scheduler::get_status(),
    })
}

fn slugify(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_and_agent_registration_share_one_interface() {
        let dir = std::env::temp_dir().join(format!("biturbo-app-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        register_agent(&state, "Test Agent".into(), "test".into(), None).unwrap();
        let payload = bootstrap(&state).unwrap();
        assert_eq!(payload.stats.total_agents, 1);
        assert_eq!(payload.agents[0].id, "test-agent");
        std::fs::remove_dir_all(dir).ok();
    }
}
