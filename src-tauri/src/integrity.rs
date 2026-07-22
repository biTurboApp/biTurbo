//! Integrity auditing and safe repair of reconstructable runtime state.

use crate::error::{BiError, BiResult};
use crate::state::AppState;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityIssue {
    pub id: String,
    pub run_id: String,
    pub project_id: Option<String>,
    pub severity: String,
    pub subsystem: String,
    pub issue_kind: String,
    pub expected_state: String,
    pub actual_state: String,
    pub recommended_action: String,
    pub safe_automatic: bool,
    pub repaired_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub id: String,
    pub project_id: Option<String>,
    pub trigger_kind: String,
    pub status: String,
    pub checked_projects: usize,
    pub issue_count: usize,
    pub repaired_count: usize,
    pub deferred_count: usize,
    pub before_summary: serde_json::Value,
    pub after_summary: Option<serde_json::Value>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub issues: Vec<IntegrityIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub status: String,
    pub checked_at: i64,
    pub project_count: usize,
    pub pending_mutations: usize,
    pub pending_candidates: usize,
    pub recoverable_operations: usize,
    pub expired_leases: usize,
    pub last_integrity_run: Option<IntegrityReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairRequest {
    pub project_id: Option<String>,
    pub integrity_run_id: String,
    pub issue_ids: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPlan {
    pub id: String,
    pub integrity_run_id: String,
    pub project_id: Option<String>,
    pub dry_run: bool,
    pub issue_ids: Vec<String>,
    pub actions: Vec<String>,
    pub repaired: usize,
    pub status: String,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenancePolicy {
    pub project_id: String,
    pub enabled: bool,
    pub interval_hours: u64,
    pub idle_delay_seconds: u64,
    pub auto_safe_repairs: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub updated_at: i64,
}

pub fn health_report(state: &AppState) -> BiResult<HealthReport> {
    let conn = state.db.conn()?;
    let project_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?;
    let pending_mutations: i64 = conn.query_row(
        "SELECT COUNT(*) FROM index_mutations WHERE applied_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    let pending_candidates: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_candidates WHERE status='pending'",
        [],
        |row| row.get(0),
    )?;
    let recoverable_operations: i64 = conn.query_row(
        "SELECT COUNT(*) FROM operations WHERE status IN ('queued','running','failed')",
        [],
        |row| row.get(0),
    )?;
    let now = chrono::Utc::now().timestamp_millis();
    let expired_leases: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runtime_leases WHERE expires_at <= ?1",
        rusqlite::params![now],
        |row| row.get(0),
    )?;
    drop(conn);
    let last_integrity_run = latest_report(state, None)?;
    let status = if last_integrity_run.as_ref().is_some_and(|report| {
        report
            .issues
            .iter()
            .any(|issue| issue.severity == "critical")
    }) {
        "critical"
    } else if pending_mutations > 0
        || expired_leases > 0
        || last_integrity_run
            .as_ref()
            .is_some_and(|report| report.issue_count > 0)
    {
        "degraded"
    } else {
        "healthy"
    };
    Ok(HealthReport {
        status: status.into(),
        checked_at: now,
        project_count: project_count as usize,
        pending_mutations: pending_mutations as usize,
        pending_candidates: pending_candidates as usize,
        recoverable_operations: recoverable_operations as usize,
        expired_leases: expired_leases as usize,
        last_integrity_run,
    })
}

pub fn run_check(
    state: &AppState,
    project_id: Option<&str>,
    trigger_kind: &str,
) -> BiResult<IntegrityReport> {
    if let Some(project_id) = project_id {
        crate::project::get(state, project_id)?;
    }
    let lease = crate::runtime::claim_lease(state, project_id, "integrity", 300)?;
    let result = run_check_inner(state, project_id, trigger_kind);
    let _ = crate::runtime::release_lease(state, &lease.lease_key);
    result
}

fn run_check_inner(
    state: &AppState,
    project_id: Option<&str>,
    trigger_kind: &str,
) -> BiResult<IntegrityReport> {
    let id = format!("integrity-{}", uuid::Uuid::new_v4());
    let started_at = chrono::Utc::now().timestamp_millis();
    let projects: Vec<String> = {
        let conn = state.db.conn()?;
        if let Some(project_id) = project_id {
            vec![project_id.to_string()]
        } else {
            let mut stmt = conn.prepare("SELECT id FROM projects ORDER BY id")?;
            let projects = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(Result::ok)
                .collect();
            projects
        }
    };
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO integrity_runs(id, project_id, trigger_kind, status, started_at)
             VALUES(?1,?2,?3,'running',?4)",
            rusqlite::params![id, project_id, trigger_kind, started_at],
        )?;
        Ok(())
    })?;

    let mut issues = Vec::new();
    for project in &projects {
        inspect_project(state, &id, project, started_at, &mut issues)?;
    }
    inspect_global(state, &id, started_at, &mut issues)?;
    let before_summary = serde_json::json!({
        "projects": projects.len(),
        "issues": issues.len(),
        "safe_automatic": issues.iter().filter(|issue| issue.safe_automatic).count(),
        "confirmation_required": issues.iter().filter(|issue| !issue.safe_automatic).count()
    });
    let finished_at = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        for issue in &issues {
            tx.execute(
                "INSERT INTO integrity_issues(id, run_id, project_id, severity, subsystem,
                     issue_kind, expected_state, actual_state, recommended_action,
                     safe_automatic, created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                rusqlite::params![
                    issue.id,
                    issue.run_id,
                    issue.project_id,
                    issue.severity,
                    issue.subsystem,
                    issue.issue_kind,
                    issue.expected_state,
                    issue.actual_state,
                    issue.recommended_action,
                    issue.safe_automatic as i64,
                    issue.created_at
                ],
            )?;
        }
        tx.execute(
            "UPDATE integrity_runs SET status='succeeded', checked_projects=?1,
                 issue_count=?2, deferred_count=?3, before_summary=?4, finished_at=?5 WHERE id=?6",
            rusqlite::params![
                projects.len() as i64,
                issues.len() as i64,
                issues.iter().filter(|issue| !issue.safe_automatic).count() as i64,
                before_summary.to_string(),
                finished_at,
                id,
            ],
        )?;
        Ok(())
    })?;
    Ok(IntegrityReport {
        id,
        project_id: project_id.map(String::from),
        trigger_kind: trigger_kind.into(),
        status: "succeeded".into(),
        checked_projects: projects.len(),
        issue_count: issues.len(),
        repaired_count: 0,
        deferred_count: issues.iter().filter(|issue| !issue.safe_automatic).count(),
        before_summary,
        after_summary: None,
        started_at,
        finished_at: Some(finished_at),
        issues,
    })
}

fn inspect_project(
    state: &AppState,
    run_id: &str,
    project_id: &str,
    now: i64,
    issues: &mut Vec<IntegrityIssue>,
) -> BiResult<()> {
    let conn = state.db.conn()?;
    let memory_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE project_id=?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let active_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE project_id=?1 AND superseded_by IS NULL",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let stored_count: i64 = conn.query_row(
        "SELECT memory_count FROM projects WHERE id=?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let fts_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories_fts WHERE project_id=?1",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM index_mutations WHERE project_id=?1 AND applied_at IS NULL",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    let expected_digest: Option<String> = conn
        .query_row(
            "SELECT content_digest FROM index_state WHERE project_id=?1",
            rusqlite::params![project_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if memory_count != stored_count {
        issues.push(issue(
            run_id,
            Some(project_id),
            "warning",
            "counters",
            "memory_count_mismatch",
            memory_count,
            stored_count,
            "recalculate project counters",
            true,
            now,
        ));
    }
    if memory_count != fts_count {
        issues.push(issue(
            run_id,
            Some(project_id),
            "warning",
            "fts",
            "fts_count_mismatch",
            memory_count,
            fts_count,
            "rebuild project FTS rows",
            true,
            now,
        ));
    }
    if pending > 0 {
        issues.push(issue(
            run_id,
            Some(project_id),
            "warning",
            "journal",
            "pending_index_mutations",
            0,
            pending,
            "replay durable index journal",
            true,
            now,
        ));
    }
    drop(conn);
    let index = state.get_or_load_index(project_id)?;
    let digest_mismatch = expected_digest
        .as_deref()
        .is_none_or(|expected| expected != index.uid_digest());
    if index.len() != active_count as usize || digest_mismatch {
        issues.push(issue(
            run_id,
            Some(project_id),
            "warning",
            "vector",
            "vector_identity_mismatch",
            format!(
                "count={active_count}, digest={}",
                expected_digest.unwrap_or_else(|| "missing".into())
            ),
            format!("count={}, digest={}", index.len(), index.uid_digest()),
            "rebuild vector index from active memories",
            true,
            now,
        ));
    }
    let conn = state.db.conn()?;
    let mismatched_receipts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM observations o WHERE o.project_id=?1 AND o.candidate_count !=
             (SELECT COUNT(*) FROM memory_candidates c WHERE c.observation_id=o.id)",
        rusqlite::params![project_id],
        |row| row.get(0),
    )?;
    if mismatched_receipts > 0 {
        issues.push(issue(
            run_id,
            Some(project_id),
            "warning",
            "capture",
            "candidate_count_mismatch",
            0,
            mismatched_receipts,
            "recalculate observation candidate counts",
            true,
            now,
        ));
    }
    Ok(())
}

fn inspect_global(
    state: &AppState,
    run_id: &str,
    now: i64,
    issues: &mut Vec<IntegrityIssue>,
) -> BiResult<()> {
    let conn = state.db.conn()?;
    let expired: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runtime_leases WHERE expires_at <= ?1",
        rusqlite::params![now],
        |row| row.get(0),
    )?;
    if expired > 0 {
        issues.push(issue(
            run_id,
            None,
            "warning",
            "runtime",
            "expired_runtime_leases",
            0,
            expired,
            "release expired leases",
            true,
            now,
        ));
    }
    Ok(())
}

fn issue(
    run_id: &str,
    project_id: Option<&str>,
    severity: &str,
    subsystem: &str,
    issue_kind: &str,
    expected: impl ToString,
    actual: impl ToString,
    action: &str,
    safe: bool,
    now: i64,
) -> IntegrityIssue {
    IntegrityIssue {
        id: format!("issue-{}", uuid::Uuid::new_v4()),
        run_id: run_id.into(),
        project_id: project_id.map(String::from),
        severity: severity.into(),
        subsystem: subsystem.into(),
        issue_kind: issue_kind.into(),
        expected_state: expected.to_string(),
        actual_state: actual.to_string(),
        recommended_action: action.into(),
        safe_automatic: safe,
        repaired_at: None,
        created_at: now,
    }
}

pub fn repair(state: &AppState, request: RepairRequest) -> BiResult<RepairPlan> {
    let report = report_by_id(state, &request.integrity_run_id)?;
    let selected: Vec<IntegrityIssue> = report
        .issues
        .into_iter()
        .filter(|issue| request.issue_ids.is_empty() || request.issue_ids.contains(&issue.id))
        .collect();
    if selected.is_empty() {
        return Err(BiError::Invalid("repair request selected no issues".into()));
    }
    if selected.iter().any(|issue| !issue.safe_automatic) {
        return Err(BiError::Invalid(
            "repair request contains semantic issues requiring explicit content review".into(),
        ));
    }
    let id = format!("repair-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().timestamp_millis();
    let actions: Vec<String> = selected
        .iter()
        .map(|issue| issue.recommended_action.clone())
        .collect();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO repair_runs(id, integrity_run_id, project_id, issue_ids, status, dry_run, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                id,
                request.integrity_run_id,
                request.project_id,
                serde_json::to_string(&selected.iter().map(|issue| &issue.id).collect::<Vec<_>>())?,
                if request.dry_run { "planned" } else { "running" },
                request.dry_run as i64,
                now,
            ],
        )?;
        Ok(())
    })?;
    if request.dry_run {
        return Ok(RepairPlan {
            id,
            integrity_run_id: request.integrity_run_id,
            project_id: request.project_id,
            dry_run: true,
            issue_ids: selected.into_iter().map(|issue| issue.id).collect(),
            actions,
            repaired: 0,
            status: "planned".into(),
            created_at: now,
            finished_at: Some(now),
        });
    }
    let mut repaired = 0;
    for issue in &selected {
        apply_safe_repair(state, issue)?;
        let repaired_at = chrono::Utc::now().timestamp_millis();
        state.db.write(|tx| {
            tx.execute(
                "UPDATE integrity_issues SET repaired_at=?1 WHERE id=?2",
                rusqlite::params![repaired_at, issue.id],
            )?;
            Ok(())
        })?;
        repaired += 1;
    }
    let finished_at = chrono::Utc::now().timestamp_millis();
    let result = serde_json::json!({"repaired": repaired, "actions": actions});
    state.db.write(|tx| {
        tx.execute(
            "UPDATE repair_runs SET status='succeeded', result=?1, finished_at=?2 WHERE id=?3",
            rusqlite::params![result.to_string(), finished_at, id],
        )?;
        tx.execute(
            "UPDATE integrity_runs SET repaired_count = repaired_count + ?1 WHERE id=?2",
            rusqlite::params![repaired as i64, request.integrity_run_id],
        )?;
        Ok(())
    })?;
    Ok(RepairPlan {
        id,
        integrity_run_id: request.integrity_run_id,
        project_id: request.project_id,
        dry_run: false,
        issue_ids: selected.into_iter().map(|issue| issue.id).collect(),
        actions,
        repaired,
        status: "succeeded".into(),
        created_at: now,
        finished_at: Some(finished_at),
    })
}

fn apply_safe_repair(state: &AppState, issue: &IntegrityIssue) -> BiResult<()> {
    match issue.issue_kind.as_str() {
        "memory_count_mismatch" => state.db.write(|tx| {
            tx.execute(
                "UPDATE projects SET memory_count=(SELECT COUNT(*) FROM memories WHERE project_id=projects.id),
                     updated_at=?1 WHERE id=?2",
                rusqlite::params![chrono::Utc::now().timestamp_millis(), issue.project_id],
            )?;
            Ok(())
        }),
        "fts_count_mismatch" => state.db.write(|tx| {
            tx.execute("DELETE FROM memories_fts WHERE project_id=?1", rusqlite::params![issue.project_id])?;
            tx.execute(
                "INSERT INTO memories_fts(uid, content, tags, mem_type, project_id)
                 SELECT uid, content, COALESCE(tags,''), mem_type, project_id FROM memories WHERE project_id=?1",
                rusqlite::params![issue.project_id],
            )?;
            Ok(())
        }),
        "pending_index_mutations" => {
            state.replay_index_mutations(issue.project_id.as_deref().ok_or_else(|| BiError::Internal("journal issue lacks project".into()))?)?;
            Ok(())
        }
        "vector_identity_mismatch" => {
            state.repair_index_if_needed(issue.project_id.as_deref().ok_or_else(|| BiError::Internal("vector issue lacks project".into()))?)
        }
        "candidate_count_mismatch" => state.db.write(|tx| {
            tx.execute(
                "UPDATE observations SET candidate_count=(SELECT COUNT(*) FROM memory_candidates WHERE observation_id=observations.id)
                 WHERE project_id=?1",
                rusqlite::params![issue.project_id],
            )?;
            Ok(())
        }),
        "expired_runtime_leases" => {
            crate::runtime::cleanup_expired_leases(state)?;
            Ok(())
        }
        other => Err(BiError::Invalid(format!("issue {other} is not automatically repairable"))),
    }
}

pub fn latest_report(
    state: &AppState,
    project_id: Option<&str>,
) -> BiResult<Option<IntegrityReport>> {
    let conn = state.db.conn()?;
    let id: Option<String> = if let Some(project_id) = project_id {
        conn.query_row(
            "SELECT id FROM integrity_runs WHERE project_id=?1 OR project_id IS NULL ORDER BY started_at DESC LIMIT 1",
            rusqlite::params![project_id],
            |row| row.get(0),
        ).optional()?
    } else {
        conn.query_row(
            "SELECT id FROM integrity_runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?
    };
    id.map(|id| report_by_id(state, &id)).transpose()
}

pub fn report_by_id(state: &AppState, id: &str) -> BiResult<IntegrityReport> {
    let conn = state.db.conn()?;
    let mut report = conn.query_row(
        "SELECT id, project_id, trigger_kind, status, checked_projects, issue_count,
                repaired_count, deferred_count, before_summary, after_summary, started_at, finished_at
         FROM integrity_runs WHERE id=?1",
        rusqlite::params![id],
        |row| {
            let before: Option<String> = row.get(8)?;
            let after: Option<String> = row.get(9)?;
            Ok(IntegrityReport {
                id: row.get(0)?, project_id: row.get(1)?, trigger_kind: row.get(2)?, status: row.get(3)?,
                checked_projects: row.get::<_, i64>(4)? as usize, issue_count: row.get::<_, i64>(5)? as usize,
                repaired_count: row.get::<_, i64>(6)? as usize, deferred_count: row.get::<_, i64>(7)? as usize,
                before_summary: before.and_then(|value| serde_json::from_str(&value).ok()).unwrap_or_else(|| serde_json::json!({})),
                after_summary: after.and_then(|value| serde_json::from_str(&value).ok()),
                started_at: row.get(10)?, finished_at: row.get(11)?, issues: Vec::new(),
            })
        },
    ).optional()?.ok_or_else(|| BiError::NotFound(format!("integrity run {id}")))?;
    let mut stmt = conn.prepare(
        "SELECT id, run_id, project_id, severity, subsystem, issue_kind, expected_state,
                actual_state, recommended_action, safe_automatic, repaired_at, created_at
         FROM integrity_issues WHERE run_id=?1 ORDER BY created_at, id",
    )?;
    report.issues = stmt
        .query_map(rusqlite::params![id], |row| {
            Ok(IntegrityIssue {
                id: row.get(0)?,
                run_id: row.get(1)?,
                project_id: row.get(2)?,
                severity: row.get(3)?,
                subsystem: row.get(4)?,
                issue_kind: row.get(5)?,
                expected_state: row.get(6)?,
                actual_state: row.get(7)?,
                recommended_action: row.get(8)?,
                safe_automatic: row.get::<_, i64>(9)? != 0,
                repaired_at: row.get(10)?,
                created_at: row.get(11)?,
            })
        })?
        .filter_map(Result::ok)
        .collect();
    Ok(report)
}

pub fn get_maintenance_policy(state: &AppState, project_id: &str) -> BiResult<MaintenancePolicy> {
    crate::project::get(state, project_id)?;
    let conn = state.db.conn()?;
    Ok(conn
        .query_row(
            "SELECT project_id, enabled, interval_hours, idle_delay_seconds, auto_safe_repairs,
                last_run_at, next_run_at, updated_at FROM maintenance_policies WHERE project_id=?1",
            rusqlite::params![project_id],
            row_to_maintenance_policy,
        )
        .optional()?
        .unwrap_or(MaintenancePolicy {
            project_id: project_id.into(),
            enabled: true,
            interval_hours: 24,
            idle_delay_seconds: 120,
            auto_safe_repairs: true,
            last_run_at: None,
            next_run_at: None,
            updated_at: 0,
        }))
}

pub fn update_maintenance_policy(
    state: &AppState,
    mut policy: MaintenancePolicy,
) -> BiResult<MaintenancePolicy> {
    crate::project::get(state, &policy.project_id)?;
    policy.interval_hours = policy.interval_hours.clamp(1, 24 * 30);
    policy.idle_delay_seconds = policy.idle_delay_seconds.clamp(0, 3_600);
    policy.updated_at = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO maintenance_policies(project_id, enabled, interval_hours, idle_delay_seconds,
                 auto_safe_repairs, last_run_at, next_run_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(project_id) DO UPDATE SET enabled=excluded.enabled,
                 interval_hours=excluded.interval_hours, idle_delay_seconds=excluded.idle_delay_seconds,
                 auto_safe_repairs=excluded.auto_safe_repairs, next_run_at=excluded.next_run_at,
                 updated_at=excluded.updated_at",
            rusqlite::params![policy.project_id, policy.enabled as i64, policy.interval_hours as i64,
                policy.idle_delay_seconds as i64, policy.auto_safe_repairs as i64,
                policy.last_run_at, policy.next_run_at, policy.updated_at],
        )?;
        Ok(())
    })?;
    get_maintenance_policy(state, &policy.project_id)
}

fn row_to_maintenance_policy(row: &rusqlite::Row<'_>) -> rusqlite::Result<MaintenancePolicy> {
    Ok(MaintenancePolicy {
        project_id: row.get(0)?,
        enabled: row.get::<_, i64>(1)? != 0,
        interval_hours: row.get::<_, i64>(2)? as u64,
        idle_delay_seconds: row.get::<_, i64>(3)? as u64,
        auto_safe_repairs: row.get::<_, i64>(4)? != 0,
        last_run_at: row.get(5)?,
        next_run_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auditor_detects_and_repairs_counter_drift() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-integrity-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        state
            .db
            .write(|tx| {
                tx.execute("UPDATE projects SET memory_count=99 WHERE id='default'", [])?;
                Ok(())
            })
            .unwrap();
        let report = run_check(&state, Some("default"), "test").unwrap();
        let issue = report
            .issues
            .iter()
            .find(|issue| issue.issue_kind == "memory_count_mismatch")
            .unwrap();
        let plan = repair(
            &state,
            RepairRequest {
                project_id: Some("default".into()),
                integrity_run_id: report.id,
                issue_ids: vec![issue.id.clone()],
                dry_run: false,
            },
        )
        .unwrap();
        assert_eq!(plan.repaired, 1);
        let project = crate::project::get(&state, "default").unwrap();
        assert_eq!(project.memory_count, 0);
    }
}
