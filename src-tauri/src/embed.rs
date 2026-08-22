use crate::error::{BiError, BiResult};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use hex;
use lru::LruCache;
use once_cell::sync::Lazy;
use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use parking_lot::{Mutex, RwLock};
use sha2::{Digest, Sha256};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing;

#[cfg(feature = "cuda")]
use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};

static EMBED_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub const DEFAULT_DIM: usize = 384;
pub const DEFAULT_MODEL: &str = "BGE-small-en-v1.5";
const QUERY_CACHE_CAP: usize = 256;
const IDLE_RELEASE: Duration = Duration::from_secs(2 * 60);
/// Explicit batch size for uncached bulk embeddings. Bounded to keep ONNX
/// arena memory low (32 texts × 512 tokens ≈ few hundred MB vs multi-GB).
const EMBED_BATCH: usize = 32;

pub struct Embedder {
    /// RwLock (not Mutex): `TextEmbedding::embed` takes `&self` and the
    /// underlying ONNX session is thread-safe, so concurrent embeds only
    /// need a read lock. The write lock is for lazy (re)init / release.
    model: Arc<RwLock<Option<TextEmbedding>>>,
    model_name: &'static str,
    pub dim: usize,
    query_cache: Arc<Mutex<LruCache<String, Vec<f32>>>>,
    last_used: Arc<Mutex<Instant>>,
}

impl Embedder {
    pub fn new(model_name: &str) -> BiResult<Self> {
        let (_model_enum, canonical, dim) = resolve_model(model_name)?;
        Ok(Self {
            // Lazy-load the ONNX model on first semantic operation instead of AppState::open.
            // This keeps MCP/UI cold start light and avoids holding model memory when the
            // caller only needs metadata, projects, settings, or non-semantic actions.
            model: Arc::new(RwLock::new(None)),
            model_name: canonical,
            dim,
            query_cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(QUERY_CACHE_CAP).unwrap(),
            ))),
            last_used: Arc::new(Mutex::new(Instant::now())),
        })
    }

    /// Ensure the model is loaded; cheap read-lock check on the hot path.
    fn ensure_model(&self) -> BiResult<()> {
        if self.model.read().is_none() {
            let mut guard = self.model.write();
            if guard.is_none() {
                let (model_enum, _, _) = resolve_model(self.model_name)?;
                *guard = Some(load_model(model_enum)?);
            }
        }
        *self.last_used.lock() = Instant::now();
        Ok(())
    }

    pub fn embed(&self, text: &str) -> BiResult<Vec<f32>> {
        let mut out = self.embed_batch(&[text])?;
        Ok(out.remove(0))
    }

    pub fn embed_batch(&self, texts: &[&str]) -> BiResult<Vec<Vec<f32>>> {
        let mut cached: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        let mut missing: Vec<(usize, &str)> = Vec::new();
        {
            let mut cache = self.query_cache.lock();
            for (i, text) in texts.iter().enumerate() {
                let key = cache_key(text);
                if let Some(v) = cache.get(&key) {
                    cached[i] = Some(v.clone());
                } else {
                    missing.push((i, *text));
                }
            }
        }

        if !missing.is_empty() {
            let missing_texts: Vec<&str> = missing.iter().map(|(_, t)| *t).collect();
            let mut computed = Vec::with_capacity(missing.len());
            self.embed_batch_uncached_stream(&missing_texts, |_chunk, results| {
                computed.extend(results);
                Ok(())
            })?;

            let mut cache = self.query_cache.lock();
            for ((idx, text), v) in missing.iter().zip(computed) {
                let key = cache_key(text);
                cache.put(key, v.clone());
                cached[*idx] = Some(v);
            }
        }

        Ok(cached.into_iter().map(|o| o.unwrap()).collect())
    }

    /// Uncached bulk embedding for large batches (e.g., project ingest).
    /// Skips the LRU cache to avoid pollution and streams results in small
    /// batches so callers can drop embeddings as soon as they are consumed.
    pub fn embed_batch_uncached(&self, texts: &[&str]) -> BiResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        self.embed_batch_uncached_stream(texts, |_chunk, results| {
            out.extend(results);
            Ok(())
        })?;
        Ok(out)
    }

    pub fn embed_batch_uncached_stream<F>(&self, texts: &[&str], mut on_batch: F) -> BiResult<()>
    where
        F: FnMut(&[&str], Vec<Vec<f32>>) -> BiResult<()>,
    {
        self.ensure_model()?;
        let _guard = EMBED_LOCK.lock();
        let guard = self.model.read();
        let model = guard
            .as_ref()
            .ok_or_else(|| BiError::Embed("model released mid-embed".into()))?;

        for chunk in texts.chunks(EMBED_BATCH) {
            let results = model
                .embed(chunk.to_vec(), Some(EMBED_BATCH))
                .map_err(|e| {
                    tracing::error!(
                        "embed: ONNX inference failed for {} texts: {e}",
                        chunk.len()
                    );
                    BiError::Embed(format!("embed_batch_uncached: {e}"))
                })?;
            on_batch(chunk, results)?;
        }

        Ok(())
    }

    pub fn release_if_idle(&self) {
        let idle_for = self.last_used.lock().elapsed();
        if idle_for < IDLE_RELEASE {
            return;
        }
        let _guard = EMBED_LOCK.lock();
        *self.model.write() = None;
        *self.last_used.lock() = Instant::now();
    }

    /// Force immediate model release — call after heavy workloads like ingest
    /// to free ONNX session memory and threads immediately.
    pub fn force_release(&self) {
        let _guard = EMBED_LOCK.lock();
        *self.model.write() = None;
        *self.last_used.lock() = Instant::now();
    }

    pub fn cache_len(&self) -> usize {
        self.query_cache.lock().len()
    }
}

fn resolve_model(name: &str) -> BiResult<(EmbeddingModel, &'static str, usize)> {
    Ok(match name {
        "BGE-small-en-v1.5" | "BGE-small-en" => (EmbeddingModel::BGESmallENV15, "BGE-small-en-v1.5", 384),
        "BGE-base-en-v1.5" | "BGE-base-en" => (EmbeddingModel::BGEBaseENV15, "BGE-base-en-v1.5", 768),
        "BGE-large-en-v1.5" | "BGE-large-en" => (EmbeddingModel::BGELargeENV15, "BGE-large-en-v1.5", 1024),
        "all-MiniLM-L6-v2" => (EmbeddingModel::AllMiniLML6V2, "all-MiniLM-L6-v2", 384),
        other => {
            return Err(BiError::Embed(format!(
                "unsupported model {other}; supported: BGE-small-en-v1.5, BGE-base-en-v1.5, BGE-large-en-v1.5, all-MiniLM-L6-v2"
            )))
        }
    })
}

/// `BITURBO_EMBED_EP`: `auto` (default) | `cuda` | `cpu`
fn embed_ep_preference() -> String {
    std::env::var("BITURBO_EMBED_EP")
        .unwrap_or_else(|_| "auto".into())
        .to_ascii_lowercase()
}

fn cpu_provider() -> ExecutionProviderDispatch {
    // CPUExecutionProvider::default() has use_arena=false — avoids multi-GB arena bloat.
    CPUExecutionProvider::default().build()
}

#[cfg(feature = "cuda")]
fn cuda_available() -> bool {
    match CUDAExecutionProvider::default().is_available() {
        Ok(true) => true,
        Ok(false) => {
            tracing::warn!(
                "embed: ORT built with CUDA but CUDAExecutionProvider not in available providers"
            );
            false
        }
        Err(e) => {
            tracing::warn!("embed: failed to query CUDA availability: {e}");
            false
        }
    }
}

fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    let pref = embed_ep_preference();
    match pref.as_str() {
        "cpu" => {
            tracing::info!("embed: BITURBO_EMBED_EP=cpu — using CPUExecutionProvider");
            vec![cpu_provider()]
        }
        #[cfg(feature = "cuda")]
        "cuda" => {
            if cuda_available() {
                tracing::info!("embed: BITURBO_EMBED_EP=cuda — CUDAExecutionProvider available");
            } else {
                tracing::warn!(
                    "embed: BITURBO_EMBED_EP=cuda but CUDA EP unavailable; will try register then fall back to CPU"
                );
            }
            vec![CUDAExecutionProvider::default().build(), cpu_provider()]
        }
        #[cfg(feature = "cuda")]
        _ => {
            // auto (and anything else): try CUDA, silent fallback to CPU via ORT EP chain.
            if cuda_available() {
                tracing::info!("embed: CUDAExecutionProvider available — preferring GPU, else CPU");
            } else {
                tracing::info!("embed: CUDA not available — using CPUExecutionProvider");
            }
            vec![CUDAExecutionProvider::default().build(), cpu_provider()]
        }
        #[cfg(not(feature = "cuda"))]
        _ => {
            if pref == "cuda" {
                tracing::warn!(
                    "embed: BITURBO_EMBED_EP=cuda but binary built without `cuda` feature; using CPU"
                );
            }
            vec![cpu_provider()]
        }
    }
}

fn load_model(model_enum: EmbeddingModel) -> BiResult<TextEmbedding> {
    let cache = dirs::cache_dir()
        .ok_or_else(|| BiError::Embed("no cache dir".into()))?
        .join("biturbo/models");

    let try_with = |providers: Vec<ExecutionProviderDispatch>| {
        let opts = InitOptions::new(model_enum.clone())
            .with_execution_providers(providers)
            .with_show_download_progress(false)
            .with_cache_dir(cache.clone());
        TextEmbedding::try_new(opts)
    };

    let providers = execution_providers();
    match try_with(providers) {
        Ok(model) => Ok(model),
        Err(e) => {
            #[cfg(feature = "cuda")]
            {
                let msg = e.to_string();
                let pref = embed_ep_preference();
                // ORT does not always soft-fallback when CUDA EP registration fails at runtime
                // (e.g. wrong libcuda on WSL). Retry CPU-only unless the user forced cuda.
                if pref != "cuda"
                    && (msg.contains("CUDA")
                        || msg.contains("cuda")
                        || msg.contains("CudaCall")
                        || msg.contains("CUDAExecutionProvider"))
                {
                    tracing::warn!(
                        "embed: CUDA init failed ({msg}); retrying with CPUExecutionProvider"
                    );
                    return try_with(vec![cpu_provider()])
                        .map_err(|e2| BiError::Embed(format!("init: {e2}")));
                }
            }
            Err(BiError::Embed(format!("init: {e}")))
        }
    }
}

fn cache_key(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_model_cpu_smoke() {
        // Force CPU so CI / machines without CUDA still pass.
        std::env::set_var("BITURBO_EMBED_EP", "cpu");
        let (model_enum, _, _) = resolve_model(DEFAULT_MODEL).unwrap();
        let model = load_model(model_enum).expect("load BGE-small on CPU");
        let out = model.embed(vec!["hello biTurbo"], Some(1)).expect("embed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), DEFAULT_DIM);
    }

    #[cfg(feature = "cuda")]
    #[test]
    fn load_model_cuda_smoke() {
        assert!(
            cuda_available(),
            "CUDAExecutionProvider not available — check CUDA/cuDNN on PATH and ORT cuda binaries"
        );
        std::env::set_var("BITURBO_EMBED_EP", "cuda");
        let (model_enum, _, _) = resolve_model(DEFAULT_MODEL).unwrap();
        let model = load_model(model_enum).expect("load BGE-small on CUDA");
        let out = model
            .embed(vec!["hello biTurbo cuda"], Some(1))
            .expect("embed");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), DEFAULT_DIM);
    }
}
