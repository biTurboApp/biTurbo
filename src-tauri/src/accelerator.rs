//! Device-wide local inference provider selection and diagnostics.

use crate::error::{BiError, BiResult};
use crate::state::AppState;
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static PERSISTED_PREFERENCE: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new("auto".into()));
static EFFECTIVE_PROVIDER: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new("uninitialized".into()));
static LAST_ERROR: Lazy<RwLock<Option<String>>> = Lazy::new(|| RwLock::new(None));
static FALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
static INFERENCE_COUNT: AtomicU64 = AtomicU64::new(0);
static INFERENCE_MICROS: AtomicU64 = AtomicU64::new(0);
static WARMUP_MILLIS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceleratorStatus {
    pub compiled_providers: Vec<String>,
    pub requested_provider: String,
    pub effective_provider: String,
    pub cuda_available: bool,
    pub initialization_error: Option<String>,
    pub onnx_runtime_version: String,
    pub model_name: String,
    pub model_dimension: usize,
    pub model_cache_present: bool,
    pub last_inference_provider: String,
    pub fallback_count: u64,
    pub warmup_ms: u64,
    pub average_batch_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceleratorPreference {
    pub provider: String,
    pub environment_override: Option<String>,
}

pub fn load_preference(db: &crate::db::Db) -> BiResult<()> {
    let conn = db.conn()?;
    let value: Option<String> = conn
        .query_row(
            "SELECT requested_provider FROM accelerator_preferences WHERE scope='device'",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(value) = value {
        validate_provider(&value)?;
        *PERSISTED_PREFERENCE.write() = value;
    }
    Ok(())
}

pub fn requested_provider() -> String {
    std::env::var("BITURBO_EMBED_EP")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| PERSISTED_PREFERENCE.read().clone())
}

pub fn persisted_preference() -> String {
    PERSISTED_PREFERENCE.read().clone()
}

pub fn preference() -> AcceleratorPreference {
    AcceleratorPreference {
        provider: persisted_preference(),
        environment_override: std::env::var("BITURBO_EMBED_EP").ok(),
    }
}

pub fn set_preference(state: &AppState, provider: &str) -> BiResult<AcceleratorPreference> {
    let provider = provider.to_ascii_lowercase();
    validate_provider(&provider)?;
    let now = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO accelerator_preferences(scope, requested_provider, updated_at)
             VALUES('device',?1,?2)
             ON CONFLICT(scope) DO UPDATE SET requested_provider=excluded.requested_provider,
                 updated_at=excluded.updated_at",
            rusqlite::params![provider, now],
        )?;
        Ok(())
    })?;
    *PERSISTED_PREFERENCE.write() = provider.clone();
    state.reset_embedders();
    Ok(preference())
}

pub fn status(state: &AppState) -> BiResult<AcceleratorStatus> {
    let requested = requested_provider();
    validate_provider(&requested)?;
    let inference_count = INFERENCE_COUNT.load(Ordering::Relaxed);
    let inference_micros = INFERENCE_MICROS.load(Ordering::Relaxed);
    let cache_dir = dirs::cache_dir().unwrap_or_default().join("biturbo/models");
    Ok(AcceleratorStatus {
        compiled_providers: compiled_providers(),
        requested_provider: requested,
        effective_provider: EFFECTIVE_PROVIDER.read().clone(),
        cuda_available: cuda_available(),
        initialization_error: LAST_ERROR.read().clone(),
        onnx_runtime_version: "2.0.0-rc.9".into(),
        model_name: crate::embed::DEFAULT_MODEL.into(),
        model_dimension: state.embedder.dim,
        model_cache_present: cache_dir.is_dir(),
        last_inference_provider: EFFECTIVE_PROVIDER.read().clone(),
        fallback_count: FALLBACK_COUNT.load(Ordering::Relaxed),
        warmup_ms: WARMUP_MILLIS.load(Ordering::Relaxed),
        average_batch_ms: if inference_count == 0 {
            0.0
        } else {
            inference_micros as f64 / inference_count as f64 / 1_000.0
        },
    })
}

pub fn validate_requested_provider() -> BiResult<String> {
    let value = requested_provider();
    validate_provider(&value)?;
    Ok(value)
}

pub fn mark_initialized(provider: &str, warmup_ms: u64) {
    *EFFECTIVE_PROVIDER.write() = provider.into();
    *LAST_ERROR.write() = None;
    WARMUP_MILLIS.store(warmup_ms, Ordering::Relaxed);
}

pub fn mark_fallback(error: &str) {
    FALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
    *EFFECTIVE_PROVIDER.write() = "cpu".into();
    *LAST_ERROR.write() = Some(error.into());
}

pub fn mark_error(error: &str) {
    *LAST_ERROR.write() = Some(error.into());
}

pub fn record_inference(elapsed: std::time::Duration) {
    INFERENCE_COUNT.fetch_add(1, Ordering::Relaxed);
    INFERENCE_MICROS.fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
}

pub fn compiled_providers() -> Vec<String> {
    #[cfg(feature = "cuda")]
    {
        vec!["cpu".into(), "cuda".into()]
    }
    #[cfg(not(feature = "cuda"))]
    {
        vec!["cpu".into()]
    }
}

#[cfg(feature = "cuda")]
pub fn cuda_available() -> bool {
    use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};
    CUDAExecutionProvider::default()
        .is_available()
        .unwrap_or(false)
}

#[cfg(not(feature = "cuda"))]
pub fn cuda_available() -> bool {
    false
}

fn validate_provider(provider: &str) -> BiResult<()> {
    if matches!(provider, "auto" | "cpu" | "cuda") {
        Ok(())
    } else {
        Err(BiError::Invalid(format!(
            "embedding provider must be auto, cpu, or cuda; got '{provider}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_environment_preference_is_reported() {
        let previous = std::env::var("BITURBO_EMBED_EP").ok();
        std::env::set_var("BITURBO_EMBED_EP", "quantum");
        assert!(validate_requested_provider().is_err());
        if let Some(previous) = previous {
            std::env::set_var("BITURBO_EMBED_EP", previous);
        } else {
            std::env::remove_var("BITURBO_EMBED_EP");
        }
    }
}
