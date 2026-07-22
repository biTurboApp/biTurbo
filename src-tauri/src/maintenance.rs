//! Autonomous maintenance loop. Runs only while a GUI or MCP process is alive.

use crate::error::BiResult;
use crate::integrity::{self, RepairRequest};
use crate::state::AppState;
use std::sync::Arc;
use std::time::Duration;

const STARTUP_DELAY: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn(state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("biturbo-maintenance".into())
        .spawn(move || {
            std::thread::sleep(STARTUP_DELAY);
            loop {
                if let Err(error) = run_due(&state) {
                    tracing::warn!("maintenance poll failed: {error}");
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
        .ok();
}

pub fn run_due(state: &AppState) -> BiResult<usize> {
    let now = chrono::Utc::now().timestamp_millis();
    let due: Vec<(String, u64, u64, bool)> = {
        let conn = state.db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT p.id,
                    COALESCE(m.interval_hours, 24),
                    COALESCE(m.idle_delay_seconds, 120),
                    COALESCE(m.auto_safe_repairs, 1)
             FROM projects p
             LEFT JOIN maintenance_policies m ON m.project_id=p.id
             WHERE COALESCE(m.enabled, 1)=1
               AND COALESCE(m.next_run_at, m.last_run_at + COALESCE(m.interval_hours,24)*3600000, 0) <= ?1",
        )?;
        let due = stmt
            .query_map(rusqlite::params![now], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? != 0,
                ))
            })?
            .filter_map(Result::ok)
            .collect();
        due
    };
    let mut completed = 0;
    for (project_id, interval_hours, idle_seconds, auto_repair) in due {
        if has_active_user_operation(state, &project_id)?
            || !is_idle(state, &project_id, idle_seconds)?
        {
            continue;
        }
        let _lease = match crate::runtime::claim_guard(
            state,
            Some(&project_id),
            "scheduled_maintenance",
            900,
        ) {
            Ok(lease) => lease,
            Err(_) => continue,
        };
        let operation = crate::operations::create(
            state,
            "scheduled_maintenance",
            Some(&project_id),
            Some(&serde_json::json!({"trigger": "overdue"})),
        )?;
        let outcome = run_one(state, &operation.id, &project_id, auto_repair);
        let finished = chrono::Utc::now().timestamp_millis();
        state.db.write(|tx| {
            tx.execute(
                "INSERT INTO maintenance_policies(project_id, enabled, interval_hours,
                     idle_delay_seconds, auto_safe_repairs, last_run_at, next_run_at, updated_at)
                 VALUES(?1,1,?2,?3,?4,?5,?6,?5)
                 ON CONFLICT(project_id) DO UPDATE SET last_run_at=excluded.last_run_at,
                     next_run_at=excluded.next_run_at, updated_at=excluded.updated_at",
                rusqlite::params![
                    project_id,
                    interval_hours as i64,
                    idle_seconds as i64,
                    auto_repair as i64,
                    finished,
                    finished + interval_hours as i64 * 3_600_000,
                ],
            )?;
            Ok(())
        })?;
        match outcome {
            Ok(()) => completed += 1,
            Err(error) => {
                let _ = crate::operations::fail(state, &operation.id, &error.to_string());
            }
        }
    }
    Ok(completed)
}

fn run_one(
    state: &AppState,
    operation_id: &str,
    project_id: &str,
    auto_repair: bool,
) -> BiResult<()> {
    crate::operations::mark_running(state, operation_id)?;
    crate::operations::update_progress(state, operation_id, "auditing", 0, 2, None)?;
    let report = integrity::run_check(state, Some(project_id), "scheduled")?;
    crate::operations::update_progress(state, operation_id, "audited", 1, 2, None)?;
    let safe_ids: Vec<String> = report
        .issues
        .iter()
        .filter(|issue| issue.safe_automatic)
        .map(|issue| issue.id.clone())
        .collect();
    let repair = if auto_repair && !safe_ids.is_empty() {
        Some(integrity::repair(
            state,
            RepairRequest {
                project_id: Some(project_id.into()),
                integrity_run_id: report.id.clone(),
                issue_ids: safe_ids,
                dry_run: false,
            },
        )?)
    } else {
        None
    };
    crate::operations::update_progress(state, operation_id, "done", 2, 2, None)?;
    crate::operations::complete(
        state,
        operation_id,
        &serde_json::json!({"report": report, "repair": repair}),
    )
}

fn has_active_user_operation(state: &AppState, project_id: &str) -> BiResult<bool> {
    let conn = state.db.conn()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM operations WHERE project_id=?1 AND status='running'
         AND kind IN ('ingest','watch_ingest','consolidate','model_rebuild','source_sync')",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn is_idle(state: &AppState, project_id: &str, idle_seconds: u64) -> BiResult<bool> {
    let conn = state.db.conn()?;
    let last_activity: Option<i64> = conn.query_row(
        "SELECT MAX(created_at) FROM activity WHERE project_id=?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    Ok(last_activity.is_none_or(|last| {
        chrono::Utc::now().timestamp_millis() - last >= idle_seconds as i64 * 1_000
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_maintenance_runs_once_and_sets_next_time() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-maintenance-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        assert_eq!(run_due(&state).unwrap(), 1);
        assert_eq!(run_due(&state).unwrap(), 0);
        let policy = integrity::get_maintenance_policy(&state, "default").unwrap();
        assert!(policy.next_run_at.is_some());
    }
}
