//! Cross-process runtime coordination shared by GUI and MCP.

use crate::error::{BiError, BiResult};
use crate::state::AppState;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

static OWNER_ID: Lazy<String> = Lazy::new(|| {
    format!(
        "{}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::new_v4()
    )
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeLease {
    pub lease_key: String,
    pub project_id: Option<String>,
    pub task_class: String,
    pub owner_id: String,
    pub heartbeat_at: i64,
    pub expires_at: i64,
    pub created_at: i64,
}

pub fn owner_id() -> &'static str {
    OWNER_ID.as_str()
}

pub fn claim_lease(
    state: &AppState,
    project_id: Option<&str>,
    task_class: &str,
    ttl_seconds: u64,
) -> BiResult<RuntimeLease> {
    if task_class.trim().is_empty() {
        return Err(BiError::Invalid("task_class is required".into()));
    }
    let project_key = project_id.unwrap_or("global");
    let lease_key = format!("{project_key}:{task_class}");
    let now = chrono::Utc::now().timestamp_millis();
    let expires_at = now + ttl_seconds.clamp(10, 3_600) as i64 * 1_000;
    state.db.write(|tx| {
        tx.execute(
            "DELETE FROM runtime_leases WHERE lease_key = ?1 AND expires_at <= ?2",
            rusqlite::params![lease_key, now],
        )?;
        let changed = tx.execute(
            "INSERT INTO runtime_leases(lease_key, project_id, task_class, owner_id,
                 heartbeat_at, expires_at, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?5)
             ON CONFLICT(lease_key) DO UPDATE SET heartbeat_at=excluded.heartbeat_at,
                 expires_at=excluded.expires_at
             WHERE runtime_leases.owner_id=excluded.owner_id",
            rusqlite::params![
                lease_key,
                project_id,
                task_class,
                owner_id(),
                now,
                expires_at
            ],
        )?;
        if changed == 0 {
            return Err(BiError::Invalid(format!(
                "task '{task_class}' is already owned by another local process"
            )));
        }
        Ok(())
    })?;
    Ok(RuntimeLease {
        lease_key,
        project_id: project_id.map(String::from),
        task_class: task_class.to_string(),
        owner_id: owner_id().to_string(),
        heartbeat_at: now,
        expires_at,
        created_at: now,
    })
}

pub fn heartbeat(state: &AppState, lease_key: &str, ttl_seconds: u64) -> BiResult<()> {
    let now = chrono::Utc::now().timestamp_millis();
    let expires = now + ttl_seconds.clamp(10, 3_600) as i64 * 1_000;
    state.db.write(|tx| {
        let changed = tx.execute(
            "UPDATE runtime_leases SET heartbeat_at=?1, expires_at=?2
             WHERE lease_key=?3 AND owner_id=?4",
            rusqlite::params![now, expires, lease_key, owner_id()],
        )?;
        if changed == 0 {
            return Err(BiError::Invalid(format!(
                "lease {lease_key} is no longer owned"
            )));
        }
        Ok(())
    })
}

pub fn release_lease(state: &AppState, lease_key: &str) -> BiResult<()> {
    state.db.write(|tx| {
        tx.execute(
            "DELETE FROM runtime_leases WHERE lease_key=?1 AND owner_id=?2",
            rusqlite::params![lease_key, owner_id()],
        )?;
        Ok(())
    })
}

pub fn cleanup_expired_leases(state: &AppState) -> BiResult<usize> {
    let now = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        Ok(tx.execute(
            "DELETE FROM runtime_leases WHERE expires_at <= ?1",
            rusqlite::params![now],
        )?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_is_reentrant_for_owner_and_exclusive_by_key() {
        let dir = std::env::temp_dir().join(format!("biturbo-lease-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        let first = claim_lease(&state, Some("default"), "integrity", 30).unwrap();
        let second = claim_lease(&state, Some("default"), "integrity", 60).unwrap();
        assert_eq!(first.lease_key, second.lease_key);
        heartbeat(&state, &first.lease_key, 60).unwrap();
        release_lease(&state, &first.lease_key).unwrap();
    }
}
