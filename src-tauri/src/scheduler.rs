use crate::consolidate::{self, ConsolidateReport};
use crate::db::log_activity;
use crate::error::BiResult;
use crate::state::AppState;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Emitter;
use tokio::sync::mpsc;

const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const STARTUP_DELAY: Duration = Duration::from_secs(60);
const JOB_CHANNEL_CAPACITY: usize = 8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConsolidateStatus {
    pub last_run_at: Option<i64>,
    pub next_run_in_secs: u64,
    pub last_report: Option<ConsolidateReport>,
    pub running: bool,
    pub interval_secs: u64,
    pub queued: bool,
}

#[derive(Default)]
struct Shared {
    last_run_at: Option<i64>,
    last_report: Option<ConsolidateReport>,
    running: bool,
    last_finish: Option<Instant>,
    /// A manual job is queued waiting for the current run to finish.
    queued: bool,
}

static STATE: once_cell::sync::Lazy<Arc<Mutex<Shared>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(Shared::default())));

/// Manual job request — `project_id == None` means "consolidate everything".
#[derive(Debug, Clone)]
pub struct ManualJob {
    pub project_id: Option<String>,
}

static JOB_TX: OnceCell<mpsc::Sender<ManualJob>> = OnceCell::new();

pub fn spawn(state: Arc<AppState>) {
    // Periodic scheduled run loop.
    let state_for_task = state.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            run_once(&state_for_task, None).await;
            tokio::time::sleep(INTERVAL).await;
        }
    });

    // Periodically release the ONNX embedding session if idle to reclaim RAM.
    let state_for_release = state.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(120));
        interval.tick().await;
        loop {
            interval.tick().await;
            state_for_release.release_idle_embedders();
        }
    });

    // Index flushing is handled by the thread spawned in AppState::open, which
    // also covers the standalone MCP binary.

    // Manual job worker. Pulls ManualJob values from the shared channel and
    // runs them serially (with the periodic loop) so we never have two
    // consolidates touching the same DB at once.
    let (manual_tx, mut manual_rx) = mpsc::channel::<ManualJob>(JOB_CHANNEL_CAPACITY);
    let _ = JOB_TX.set(manual_tx);

    let state_for_worker = state.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(job) = manual_rx.recv().await {
            {
                let mut g = STATE.lock();
                g.queued = false;
            }
            run_once(&state_for_worker, Some(job)).await;
        }
    });
}

/// Non-blocking manual trigger. Returns immediately. The result is observed
/// later via `consolidate_status` or the `consolidate:done` Tauri event.
pub fn enqueue(state: &AppState, project_id: Option<String>) -> BiResult<()> {
    // Verify the project exists if one was specified. Done synchronously so we
    // fail fast on bad input — this is a cheap PK lookup, not the heavy work.
    if let Some(pid) = project_id.as_deref() {
        crate::project::get(state, pid).map_err(|_| {
            crate::error::BiError::Invalid(format!("project '{pid}' does not exist"))
        })?;
    }

    let mut g = STATE.lock();
    if g.running {
        // Coalesce: one manual job is enough while another runs. Mark queued.
        g.queued = true;
        return Ok(());
    }
    drop(g);

    let tx = match JOB_TX.get() {
        Some(tx) => tx.clone(),
        None => {
            return Err(crate::error::BiError::Internal(
                "consolidate worker has not been started yet".into(),
            ));
        }
    };
    match tx.try_send(ManualJob { project_id }) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            let mut g = STATE.lock();
            g.queued = true;
            Ok(())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(crate::error::BiError::Internal(
            "consolidate worker is not running".into(),
        )),
    }
}

/// Backwards-compat alias used by the periodic loop and by the original
/// synchronous call from `commands::consolidate_now` (we no longer use this
/// path for the IPC command, but keep it in case other callers want to block
/// on the result — e.g. tests).
pub fn run_now_blocking(state: &AppState) -> BiResult<ConsolidateReport> {
    let report = consolidate::consolidate(state, None)?;
    let now = chrono::Utc::now().timestamp_millis();
    {
        let mut g = STATE.lock();
        g.last_run_at = Some(now);
        g.last_report = Some(report.clone());
        g.running = false;
        g.last_finish = Some(Instant::now());
    }
    state.db.write(|tx| {
        log_activity(
            tx,
            None,
            None,
            "consolidate",
            None,
            Some(&serde_json::to_value(&report)?),
        )?;
        Ok(())
    })?;
    Ok(report)
}

pub fn get_status() -> ConsolidateStatus {
    let g = STATE.lock();
    let next = if g.running || g.queued {
        0
    } else if let Some(finish) = g.last_finish {
        INTERVAL
            .as_secs()
            .saturating_sub(finish.elapsed().as_secs())
    } else {
        INTERVAL.as_secs()
    };
    ConsolidateStatus {
        last_run_at: g.last_run_at,
        next_run_in_secs: next,
        last_report: g.last_report.clone(),
        running: g.running,
        interval_secs: INTERVAL.as_secs(),
        queued: g.queued,
    }
}

async fn run_once(state: &AppState, job: Option<ManualJob>) {
    {
        let mut g = STATE.lock();
        if g.running {
            // Periodic loop stumbled onto a running job — drop it. The manual
            // worker is the only producer when `job.is_some()`.
            if job.is_none() {
                return;
            }
            // Should not happen for manual jobs (we gate on `running` before
            // sending), but bail safely.
            return;
        }
        g.running = true;
    }

    let project_id = job.as_ref().and_then(|j| j.project_id.as_deref());
    let result = consolidate::consolidate(state, project_id);
    let now = chrono::Utc::now().timestamp_millis();

    {
        let mut g = STATE.lock();
        g.running = false;
        g.last_finish = Some(Instant::now());
    }

    match result {
        Ok(r) => {
            {
                let mut g = STATE.lock();
                g.last_run_at = Some(now);
                g.last_report = Some(r.clone());
            }
            if let Some(pid) = project_id {
                let _ = state.db.write(|tx| {
                    log_activity(
                        tx,
                        Some(pid),
                        None,
                        "consolidate",
                        None,
                        Some(&serde_json::to_value(&r)?),
                    )?;
                    Ok(())
                });
            } else {
                let _ = state.db.write(|tx| {
                    log_activity(
                        tx,
                        None,
                        None,
                        "consolidate",
                        None,
                        Some(&serde_json::to_value(&r)?),
                    )?;
                    Ok(())
                });
            }
            if let Some(app) = &state.app {
                let _ = app.emit("consolidate:done", &r);
            }
        }
        Err(e) => {
            tracing::warn!("consolidate run failed: {e}");
        }
    }
}
