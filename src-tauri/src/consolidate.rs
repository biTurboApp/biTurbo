use crate::db::log_activity;
use crate::error::BiResult;
use crate::memory;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConsolidateReport {
    pub decayed: usize,
    pub duplicates_found: usize,
    pub merged: usize,
    pub removed: usize,
}

pub fn consolidate(state: &AppState, project_id: Option<&str>) -> BiResult<ConsolidateReport> {
    let mut report = ConsolidateReport::default();

    report.decayed = apply_decay(state, project_id)?;
    let dupes = find_duplicates(state, project_id)?;
    report.duplicates_found = dupes.len();
    for (keep_uid, drop_uid) in dupes {
        if merge_pair(state, &keep_uid, &drop_uid)? {
            report.merged += 1;
            report.removed += 1;
        }
    }

    state.db.write(|tx| {
        log_activity(
            tx,
            project_id,
            None,
            "consolidate",
            None,
            Some(&serde_json::to_value(&report)?),
        )?;
        Ok(())
    })?;

    Ok(report)
}

/// Pure decay computation (issue #435): importance is always recomputed from
/// the fixed `decay_base` baseline, never from the already-decayed stored
/// value, so repeated consolidate runs converge instead of compounding.
fn decayed_importance(
    base: f64,
    created_at: i64,
    now: i64,
    access_count: i64,
    last_access: i64,
) -> f32 {
    const HALF_LIFE_MS: f64 = 60.0 * 24.0 * 3600.0 * 1000.0;
    const RECENT_WINDOW_MS: i64 = 30 * 24 * 3600 * 1000;
    let age_ms = (now - created_at).max(0) as f64;
    let decay = (-age_ms / HALF_LIFE_MS).exp();
    let recent_access = (now - last_access) < RECENT_WINDOW_MS;
    let boost = if recent_access {
        (access_count as f64 * 0.05).min(0.3)
    } else {
        0.0
    };
    (base * decay + boost).clamp(0.05, 1.0) as f32
}

fn apply_decay(state: &AppState, project_id: Option<&str>) -> BiResult<usize> {
    let now = chrono::Utc::now().timestamp_millis();
    let conn = state.db.conn()?;

    let rows: Vec<(String, f64, Option<f64>, i64, i64, i64)> = match project_id {
        Some(p) => {
            let mut stmt = conn.prepare(
                "SELECT uid, importance, decay_base, created_at, access_count, last_access
                 FROM memories WHERE project_id = ?1",
            )?;
            let v: Vec<_> = stmt
                .query_map(rusqlite::params![p], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            v
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT uid, importance, decay_base, created_at, access_count, last_access
                 FROM memories",
            )?;
            let v: Vec<_> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);
            v
        }
    };
    // Compute new importances first, then apply every change inside ONE
    // transaction with a cached statement — previously each row was its own
    // autocommit transaction.
    let mut updates: Vec<(String, f32)> = Vec::new();
    for (uid, importance, decay_base, created_at, access_count, last_access) in rows {
        // Effective baseline: rows predating the decay_base backfill fall back
        // to the stored importance.
        let base = decay_base.unwrap_or(importance);
        let new_imp = decayed_importance(base, created_at, now, access_count, last_access);
        if (new_imp - importance as f32).abs() > 0.001 {
            updates.push((uid, new_imp));
        }
    }
    drop(conn);

    let touched = updates.len();
    if !updates.is_empty() {
        state.db.write(|tx| {
            let mut stmt =
                tx.prepare_cached("UPDATE memories SET importance = ?1 WHERE uid = ?2")?;
            for (uid, new_imp) in &updates {
                stmt.execute(rusqlite::params![new_imp, uid])?;
            }
            Ok(())
        })?;
    }
    Ok(touched)
}

fn find_duplicates(state: &AppState, project_id: Option<&str>) -> BiResult<Vec<(String, String)>> {
    let conn = state.db.conn()?;
    let project_ids: Vec<String> = match project_id {
        Some(p) => vec![p.to_string()],
        None => {
            let mut s = conn.prepare("SELECT DISTINCT project_id FROM memories")?;
            let v: Vec<String> = s
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            drop(s);
            v
        }
    };
    drop(conn);

    let mut dupes: HashSet<(String, String)> = HashSet::new();
    for pid in project_ids {
        // Process in batches to bound RAM. Skip code-type memories —
        // deduplicating code chunks is expensive and rarely useful.
        let idx = state.get_or_load_index(&pid)?;
        let mut offset = 0usize;
        const BATCH: usize = 1000;
        loop {
            // Page candidates directly (issue #436): memory::list has no
            // superseded_by filter, so it could pair an active memory with a
            // zombie copy. Only active rows are eligible for merging.
            let rows: Vec<(String, String, f64)> = {
                let conn = state.db.conn()?;
                let mut stmt = conn.prepare_cached(
                    "SELECT uid, content, importance FROM memories
                     WHERE project_id = ?1 AND superseded_by IS NULL AND mem_type != 'code'
                     ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
                )?;
                let v: Vec<_> = stmt
                    .query_map(rusqlite::params![pid, BATCH as i64, offset as i64], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, f64>(2)?,
                        ))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                v
            };
            if rows.is_empty() {
                break;
            }
            let by_uid: std::collections::HashMap<&str, &(String, String, f64)> =
                rows.iter().map(|r| (r.0.as_str(), r)).collect();
            let texts: Vec<&str> = rows.iter().map(|r| r.1.as_str()).collect();
            let embeddings = state.embedder.embed_batch(&texts)?;
            for (i, vec) in embeddings.iter().enumerate() {
                let a = &rows[i];
                let hits = idx.search(vec, 5, None)?;
                for h in hits {
                    if h.score < 0.95 || h.uid == a.0 {
                        continue;
                    }
                    if let Some(b) = by_uid.get(h.uid.as_str()) {
                        let (keep, drop_) = if a.2 >= b.2 {
                            (a.0.clone(), b.0.clone())
                        } else {
                            (b.0.clone(), a.0.clone())
                        };
                        dupes.insert((keep, drop_));
                    }
                }
            }
            offset += BATCH;
            if rows.len() < BATCH {
                break;
            }
        }
    }
    Ok(dupes.into_iter().collect())
}

fn merge_pair(state: &AppState, keep_uid: &str, drop_uid: &str) -> BiResult<bool> {
    let keep = memory::get(state, keep_uid)?;
    let drop_ = memory::get(state, drop_uid)?;
    let (Some(keep), Some(_drop_)) = (keep, drop_) else {
        return Ok(false);
    };

    let mut merged_tags: Vec<String> = keep.tags.clone();
    for t in &_drop_.tags {
        if !merged_tags.contains(t) {
            merged_tags.push(t.clone());
        }
    }
    memory::update(
        state,
        keep_uid,
        crate::memory::UpdateInput {
            tags: Some(merged_tags),
            ..Default::default()
        },
    )?;

    let now = chrono::Utc::now().timestamp_millis();
    // Guarded supersede (issue #436): the UPDATE only fires while BOTH sides
    // are still active. If a concurrent consolidate already superseded either
    // the keep or the drop copy, 0 rows match and we bail without forgetting
    // anything — this prevents merging a zombie into an active memory or vice
    // versa when two consolidates race.
    let superseded = state.db.write(|tx| {
        let n = tx.execute(
            "UPDATE memories SET superseded_by = (SELECT id FROM memories WHERE uid = ?1),
                                 updated_at = ?2
             WHERE uid = ?3 AND superseded_by IS NULL
               AND EXISTS(SELECT 1 FROM memories k WHERE k.uid = ?1 AND k.superseded_by IS NULL)",
            rusqlite::params![keep_uid, now, drop_uid],
        )?;
        Ok(n)
    })?;
    if superseded == 0 {
        return Ok(false);
    }
    memory::forget(state, drop_uid)?;
    Ok(true)
}

#[cfg(test)]
mod decay_tests {
    use super::decayed_importance;

    const DAY_MS: i64 = 24 * 3600 * 1000;
    /// Fixed epoch: 2026-01-01T00:00:00Z — keeps tests deterministic.
    const NOW: i64 = 1_767_225_600_000;

    #[test]
    fn identical_inputs_give_identical_results() {
        let a = decayed_importance(0.8, NOW - 10 * DAY_MS, NOW, 3, NOW - DAY_MS);
        let b = decayed_importance(0.8, NOW - 10 * DAY_MS, NOW, 3, NOW - DAY_MS);
        assert_eq!(a, b);
    }

    #[test]
    fn repeated_runs_converge_instead_of_compounding() {
        // Old bug: each run multiplied the TOTAL decay since creation into
        // the already-decayed stored value, so importance kept shrinking
        // run over run. Now every run recomputes from the immutable base,
        // so repeated applications are stable at the same value.
        let base = 0.9f64;
        let created_at = NOW - 20 * DAY_MS;
        let decay_20d = (-20.0f64 / 60.0f64).exp();
        let expected = (base * decay_20d) as f32;
        let first = decayed_importance(base, created_at, NOW, 0, created_at);
        let second = decayed_importance(base, created_at, NOW, 0, created_at);
        assert_eq!(first, second, "repeated consolidate runs must be stable");
        assert!((first - expected).abs() < 1e-6, "{first} vs {expected}");
        // Contrast — what the old compounding formula would produce on the
        // second run (decaying the already-decayed value again):
        let compounded_again = ((first as f64) * decay_20d) as f32;
        assert!(compounded_again < first);
    }

    #[test]
    fn floor_and_boost_cap_respected() {
        // Floor: tiny base, ancient memory -> clamped at 0.05.
        assert_eq!(
            decayed_importance(0.01, NOW - 1000 * DAY_MS, NOW, 0, NOW - 1000 * DAY_MS),
            0.05
        );
        // Boost cap: 100 accesses * 0.05 would be 5.0 but caps at 0.3.
        assert_eq!(decayed_importance(0.0, NOW, NOW, 100, NOW), 0.3);
        // Combined: base*decay + capped boost still clamps to [0.05, 1.0].
        assert_eq!(decayed_importance(0.9, NOW, NOW, 100, NOW), 1.0);
    }

    #[test]
    fn old_memory_with_zero_accesses_decays_toward_floor() {
        let fresh = decayed_importance(1.0, NOW, NOW, 0, NOW);
        // One decay constant (60 days): e^-1 ≈ 0.368, strictly decaying.
        let aged = decayed_importance(1.0, NOW - 60 * DAY_MS, NOW, 0, NOW - 60 * DAY_MS);
        let ancient = decayed_importance(1.0, NOW - 4000 * DAY_MS, NOW, 0, NOW - 4000 * DAY_MS);
        assert!(fresh > aged && aged > 0.1);
        assert_eq!(ancient, 0.05);
    }
}
