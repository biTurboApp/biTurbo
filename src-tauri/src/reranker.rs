//! Optional, explicitly installed local cross-encoder reranking.

use crate::error::{BiError, BiResult};
use crate::memory::MemoryWithScore;
use crate::state::AppState;
use fastembed::{
    RerankInitOptionsUserDefined, TextRerank, TokenizerFiles, UserDefinedRerankingModel,
};
use once_cell::sync::Lazy;
use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::Arc;

#[cfg(feature = "cuda")]
use ort::execution_providers::CUDAExecutionProvider;

pub const MODEL_NAME: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";
pub const MODEL_VERSION: &str = "c5ee24cb16019beea0893ab7796b1df96625c6b8";
pub const MODEL_SHA256: &str = "5d3e70fd0c9ff14b9b5169a51e957b7a9c74897afd0a35ce4bd318150c1d4d4a";
pub const MODEL_SIZE: u64 = 91_011_230;
pub const MODEL_LICENSE: &str = "Apache-2.0";
const FILES: &[&str] = &[
    "onnx/model.onnx",
    "tokenizer.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "config.json",
];

static MODEL: Lazy<Mutex<Option<Arc<TextRerank>>>> = Lazy::new(|| Mutex::new(None));
static LAST_ERROR: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
static LAST_APPLIED: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerStatus {
    pub model_name: String,
    pub version: String,
    pub license: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub installed: bool,
    pub enabled: bool,
    pub loaded: bool,
    pub artifact_path: String,
    pub last_error: Option<String>,
    pub last_recall_applied: bool,
}

pub fn status(state: &AppState) -> BiResult<RerankerStatus> {
    let dir = artifact_dir(state);
    let installed = verify_artifact(&dir).is_ok();
    let conn = state.db.conn()?;
    let enabled: bool = conn
        .query_row(
            "SELECT enabled FROM model_artifacts WHERE kind='reranker' AND model_name=?1 AND version=?2",
            rusqlite::params![MODEL_NAME, MODEL_VERSION],
            |row| Ok(row.get::<_, i64>(0)? != 0),
        )
        .unwrap_or(false);
    Ok(RerankerStatus {
        model_name: MODEL_NAME.into(),
        version: MODEL_VERSION.into(),
        license: MODEL_LICENSE.into(),
        size_bytes: MODEL_SIZE,
        sha256: MODEL_SHA256.into(),
        installed,
        enabled,
        loaded: MODEL.lock().is_some(),
        artifact_path: dir.to_string_lossy().into(),
        last_error: LAST_ERROR.lock().clone(),
        last_recall_applied: *LAST_APPLIED.lock(),
    })
}

pub fn set_enabled(state: &AppState, enabled: bool) -> BiResult<RerankerStatus> {
    let dir = artifact_dir(state);
    if enabled {
        verify_artifact(&dir)?;
    }
    let now = chrono::Utc::now().timestamp_millis();
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO model_artifacts(id, kind, model_name, version, path, sha256,
                 size_bytes, status, enabled, updated_at)
             VALUES('reranker-default','reranker',?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET path=excluded.path, status=excluded.status,
                 enabled=excluded.enabled, error=NULL, updated_at=excluded.updated_at",
            rusqlite::params![
                MODEL_NAME,
                MODEL_VERSION,
                dir.to_string_lossy(),
                MODEL_SHA256,
                MODEL_SIZE as i64,
                if dir.is_dir() { "installed" } else { "missing" },
                enabled as i64,
                now,
            ],
        )?;
        Ok(())
    })?;
    if !enabled {
        *MODEL.lock() = None;
    }
    status(state)
}

pub fn start_download(state: &AppState) -> BiResult<crate::operations::Operation> {
    let operation = crate::operations::create(
        state,
        "reranker_download",
        None,
        Some(&serde_json::json!({"model": MODEL_NAME, "version": MODEL_VERSION})),
    )?;
    let state = Arc::new(state.clone());
    let id = operation.id.clone();
    std::thread::Builder::new()
        .name(format!("biturbo-operation-{id}"))
        .spawn(move || {
            if let Err(error) = download(&state, &id) {
                let _ = record_error(&state, &error.to_string());
                let _ = crate::operations::fail(&state, &id, &error.to_string());
            }
        })
        .map_err(|error| BiError::Internal(format!("spawn reranker download: {error}")))?;
    Ok(operation)
}

fn download(state: &AppState, operation_id: &str) -> BiResult<()> {
    crate::operations::mark_running(state, operation_id)?;
    let destination = artifact_dir(state);
    let temp = destination.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp.join("onnx"))?;
    for (index, file) in FILES.iter().enumerate() {
        if crate::operations::is_cancel_requested(state, operation_id)? {
            let _ = std::fs::remove_dir_all(&temp);
            return crate::operations::mark_cancelled(state, operation_id);
        }
        crate::operations::update_progress(
            state,
            operation_id,
            "downloading_model",
            index,
            FILES.len(),
            Some(&serde_json::json!({"file": file})),
        )?;
        let url = format!(
            "https://huggingface.co/{MODEL_NAME}/resolve/{MODEL_VERSION}/{file}?download=true"
        );
        let response = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(120))
            .call()
            .map_err(|error| BiError::Io(format!("download {file}: {error}")))?;
        let target = temp.join(file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut reader = response.into_reader();
        let mut output = std::fs::File::create(&target)?;
        std::io::copy(&mut reader, &mut output)?;
        output.flush()?;
    }
    verify_artifact(&temp)?;
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&temp, &destination)?;
    set_enabled(state, false)?;
    crate::operations::update_progress(
        state,
        operation_id,
        "verified",
        FILES.len(),
        FILES.len(),
        None,
    )?;
    crate::operations::complete(state, operation_id, &serde_json::to_value(status(state)?)?)
}

pub fn import_artifact(state: &AppState, source: &std::path::Path) -> BiResult<RerankerStatus> {
    verify_artifact(source)?;
    let destination = artifact_dir(state);
    let temp = destination.with_extension(format!("import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(temp.join("onnx"))?;
    for file in FILES {
        let target = temp.join(file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source.join(file), target)?;
    }
    verify_artifact(&temp)?;
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(temp, &destination)?;
    set_enabled(state, false)
}

pub fn rerank_if_enabled(
    state: &AppState,
    query: &str,
    mut hits: Vec<MemoryWithScore>,
) -> Vec<MemoryWithScore> {
    *LAST_APPLIED.lock() = false;
    if hits.len() < 2 || !status(state).is_ok_and(|value| value.enabled && value.installed) {
        return hits;
    }
    let model = match load_model(state) {
        Ok(model) => model,
        Err(error) => {
            *LAST_ERROR.lock() = Some(error.to_string());
            return hits;
        }
    };
    let count = hits.len().min(50);
    let documents: Vec<String> = hits
        .iter()
        .take(count)
        .map(|hit| hit.memory.content.clone())
        .collect();
    match model.rerank(query.to_string(), documents, false, Some(8)) {
        Ok(results) => {
            let original = hits[..count].to_vec();
            let mut reranked = Vec::with_capacity(hits.len());
            for result in results {
                if let Some(mut hit) = original.get(result.index).cloned() {
                    hit.score = result.score + hit.score * 0.001;
                    reranked.push(hit);
                }
            }
            reranked.extend(hits.drain(count..));
            *LAST_APPLIED.lock() = true;
            *LAST_ERROR.lock() = None;
            reranked
        }
        Err(error) => {
            *LAST_ERROR.lock() = Some(error.to_string());
            hits
        }
    }
}

fn load_model(state: &AppState) -> BiResult<Arc<TextRerank>> {
    if let Some(model) = MODEL.lock().clone() {
        return Ok(model);
    }
    let dir = artifact_dir(state);
    verify_artifact(&dir)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: std::fs::read(dir.join("tokenizer.json"))?,
        config_file: std::fs::read(dir.join("config.json"))?,
        special_tokens_map_file: std::fs::read(dir.join("special_tokens_map.json"))?,
        tokenizer_config_file: std::fs::read(dir.join("tokenizer_config.json"))?,
    };
    let model = UserDefinedRerankingModel::new(dir.join("onnx/model.onnx"), tokenizer_files);
    let providers = reranker_providers();
    let mut options = RerankInitOptionsUserDefined::default();
    options.execution_providers = providers;
    options.max_length = 512;
    let reranker = TextRerank::try_new_from_user_defined(model, options)
        .map_err(|error| BiError::Embed(format!("reranker initialization failed: {error}")))?;
    let reranker = Arc::new(reranker);
    *MODEL.lock() = Some(reranker.clone());
    Ok(reranker)
}

fn reranker_providers() -> Vec<ExecutionProviderDispatch> {
    #[cfg(feature = "cuda")]
    {
        let requested = crate::accelerator::requested_provider();
        if requested != "cpu" && crate::accelerator::cuda_available() {
            return vec![
                CUDAExecutionProvider::default().build(),
                CPUExecutionProvider::default().build(),
            ];
        }
    }
    vec![CPUExecutionProvider::default().build()]
}

fn artifact_dir(state: &AppState) -> std::path::PathBuf {
    state
        .data_dir
        .join("models")
        .join("reranker")
        .join("ms-marco-MiniLM-L-6-v2")
}

fn verify_artifact(dir: &std::path::Path) -> BiResult<()> {
    for file in FILES {
        if !dir.join(file).is_file() {
            return Err(BiError::NotFound(format!(
                "reranker artifact is missing {file}"
            )));
        }
    }
    let mut file = std::fs::File::open(dir.join("onnx/model.onnx"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != MODEL_SHA256 {
        return Err(BiError::Invalid(format!(
            "reranker checksum mismatch: expected {MODEL_SHA256}, got {actual}"
        )));
    }
    Ok(())
}

fn record_error(state: &AppState, error: &str) -> BiResult<()> {
    *LAST_ERROR.lock() = Some(error.into());
    state.db.write(|tx| {
        tx.execute(
            "INSERT INTO model_artifacts(id, kind, model_name, version, status, enabled, error, updated_at)
             VALUES('reranker-default','reranker',?1,?2,'failed',0,?3,?4)
             ON CONFLICT(id) DO UPDATE SET status='failed', error=excluded.error,
                 updated_at=excluded.updated_at",
            rusqlite::params![
                MODEL_NAME,
                MODEL_VERSION,
                error,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_keeps_reranker_disabled() {
        let dir =
            std::env::temp_dir().join(format!("biturbo-reranker-test-{}", uuid::Uuid::new_v4()));
        let state = AppState::open(&dir).unwrap();
        let status = status(&state).unwrap();
        assert!(!status.installed);
        assert!(!status.enabled);
        assert!(set_enabled(&state, true).is_err());
    }
}
