#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Rust-native semantic embeddings, retaining the historical facade name.
//!
//! BAAI/bge-small-en-v1.5 runs through Candle's BERT implementation: the
//! pretrained CLS token is L2 normalized, producing 384-dimensional vectors.
//! No ONNX Runtime, Python, native model runtime or synthetic hash fallback is
//! involved. The MIT-licensed model revision is pinned; artifacts are loaded
//! lazily through the existing bounded HTTP transport. CPU is the portable
//! default. Existing public `embed`, `embed_one`, and `load_failed` APIs remain.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use susi_error::{EaiError, EaiResult};
use susi_vendor_candle::candle_core::{DType, Device, IndexOp, Tensor};
use susi_vendor_candle::candle_nn::VarBuilder;
use susi_vendor_candle::candle_transformers::models::bert::{BertModel, Config};
use susi_vendor_candle::tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

const DEFAULT_MODEL_SUBDIR: &str = "BAAI--bge-small-en-v1.5--5c38ec7c";
const DEFAULT_MODEL_BASE_URL: &str = "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a";
const MAX_MODEL_FILE_BYTES: u64 = 256 * 1024 * 1024;
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
const MAX_REDIRECTS: u32 = 5;
const BATCH_SIZE: usize = 32;
const EMBEDDING_DIM: usize = 384;

struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
}

type ModelCell = OnceLock<Result<Mutex<Embedder>, String>>;

fn model_cell() -> &'static ModelCell {
    static MODEL: ModelCell = OnceLock::new();
    &MODEL
}

fn cache_dir_from(env_override: Option<&str>) -> PathBuf {
    PathBuf::from(
        env_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(".fastembed_cache"),
    )
    .join(DEFAULT_MODEL_SUBDIR)
}

fn base_url_from(env_override: Option<&str>) -> String {
    env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MODEL_BASE_URL)
        .trim_end_matches('/')
        .to_owned()
}

fn ensure_cached_file(cache: &Path, base: &str, name: &str) -> Result<Vec<u8>, String> {
    let path = cache.join(name);
    match std::fs::File::open(&path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(MAX_MODEL_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            if bytes.len() as u64 > MAX_MODEL_FILE_BYTES {
                return Err(format!(
                    "cached artifact exceeds size limit: {}",
                    path.display()
                ));
            }
            if !bytes.is_empty() {
                return Ok(bytes);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("opening {}: {error}", path.display())),
    }
    let url = format!("{base}/{name}");
    let response =
        susi_http_transport::http_call("GET", &url, &[], DOWNLOAD_TIMEOUT_SECS, MAX_REDIRECTS)
            .map_err(|e| format!("failed to fetch {url}: {e}"))?;
    if !(200..300).contains(&response.status) {
        return Err(format!("fetching {url} returned HTTP {}", response.status));
    }
    let bytes = response
        .into_bytes(MAX_MODEL_FILE_BYTES)
        .map_err(|e| format!("reading {url}: {e}"))?;
    if bytes.is_empty() {
        return Err(format!("empty model artifact: {url}"));
    }
    std::fs::create_dir_all(cache).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let pending = cache.join(format!(".{name}.{}.{stamp}.partial", std::process::id()));
    std::fs::write(&pending, &bytes).map_err(|e| e.to_string())?;
    if let Err(error) = std::fs::rename(&pending, &path) {
        let _ = std::fs::remove_file(&pending);
        if !path.is_file() {
            return Err(format!("publishing model cache: {error}"));
        }
    }
    Ok(bytes)
}

impl Embedder {
    fn load(cache: &Path, base: &str) -> Result<Self, String> {
        let config: Config =
            serde_json::from_slice(&ensure_cached_file(cache, base, "config.json")?)
                .map_err(|e| format!("invalid BERT config: {e}"))?;
        if config.hidden_size != EMBEDDING_DIM || config.max_position_embeddings < 2 {
            return Err("expected the 384-dimensional BGE small model".into());
        }
        let mut tokenizer =
            Tokenizer::from_bytes(ensure_cached_file(cache, base, "tokenizer.json")?)
                .map_err(|e| format!("invalid tokenizer: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id: u32::try_from(config.pad_token_id).map_err(|e| e.to_string())?,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: config.max_position_embeddings.min(512),
                ..Default::default()
            }))
            .map_err(|e| e.to_string())?;
        let weights = ensure_cached_file(cache, base, "model.safetensors")?;
        let variables = VarBuilder::from_buffered_safetensors(weights, DType::F32, &Device::Cpu)
            .map_err(|e| format!("invalid model weights: {e}"))?;
        let model = BertModel::load(variables, &config).map_err(|e| e.to_string())?;
        Ok(Self { model, tokenizer })
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut results = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH_SIZE) {
            let encoded = self
                .tokenizer
                .encode_batch(batch.to_vec(), true)
                .map_err(|e| e.to_string())?;
            let ids: Vec<_> = encoded
                .iter()
                .map(|item| Tensor::new(item.get_ids(), &Device::Cpu))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let types: Vec<_> = encoded
                .iter()
                .map(|item| Tensor::new(item.get_type_ids(), &Device::Cpu))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let masks: Vec<_> = encoded
                .iter()
                .map(|item| Tensor::new(item.get_attention_mask(), &Device::Cpu))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let output = (|| {
                self.model
                    .forward(
                        &Tensor::stack(&ids, 0)?,
                        &Tensor::stack(&types, 0)?,
                        Some(&Tensor::stack(&masks, 0)?),
                    )?
                    .i((.., 0, ..))?
                    .to_vec2::<f32>()
            })()
            .map_err(|e: susi_vendor_candle::candle_core::Error| e.to_string())?;
            for vector in output {
                results.push(normalize(vector)?);
            }
        }
        Ok(results)
    }
}

fn normalize(mut vector: Vec<f32>) -> Result<Vec<f32>, String> {
    if vector.len() != EMBEDDING_DIM || vector.iter().any(|value| !value.is_finite()) {
        return Err("invalid BGE embedding shape or nonfinite values".into());
    }
    let norm = vector
        .iter()
        .map(|value| f64::from(*value).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm <= f64::EPSILON {
        return Err("BGE returned a zero embedding".into());
    }
    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    Ok(vector)
}

fn model() -> EaiResult<&'static Mutex<Embedder>> {
    model_cell()
        .get_or_init(|| {
            let cache = cache_dir_from(std::env::var("FASTEMBED_CACHE_DIR").ok().as_deref());
            let base = base_url_from(std::env::var("SUSI_FASTEMBED_MODEL_URL").ok().as_deref());
            Embedder::load(&cache, &base).map(Mutex::new)
        })
        .as_ref()
        .map_err(|error| EaiError::inference(format!("embedder unavailable: {error}")))
}

/// Whether initialization has already failed. Loading remains lazy.
#[must_use]
pub fn load_failed() -> bool {
    model_cell().get().is_some_and(Result::is_err)
}

/// Compute one normalized semantic vector per input, preserving input order.
/// Empty batches return immediately without downloading a model.
pub fn embed(texts: Vec<String>) -> EaiResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    model()?
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .embed(&texts)
        .map_err(|error| EaiError::inference(format!("embed failed: {error}")))
}

/// Compute one normalized semantic embedding.
pub fn embed_one(text: &str) -> EaiResult<Vec<f32>> {
    embed(vec![text.to_owned()])?
        .pop()
        .ok_or_else(|| EaiError::inference("embedder returned no vector"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batch_does_not_initialize_or_download() {
        assert!(embed(Vec::new()).unwrap().is_empty());
        assert!(!load_failed());
    }

    #[test]
    fn normalization_rejects_bad_results_and_preserves_direction() {
        assert!(normalize(vec![0.; EMBEDDING_DIM]).is_err());
        assert!(normalize(vec![1.; 3]).is_err());
        assert!(normalize(vec![f32::NAN; EMBEDDING_DIM]).is_err());
        let mut vector = vec![0.; EMBEDDING_DIM];
        vector[0] = 3.;
        vector[1] = 4.;
        let normalized = normalize(vector).unwrap();
        assert!((normalized[0] - 0.6).abs() < 1e-6);
        assert!((normalized[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn cached_artifacts_do_not_need_network() {
        let dir = std::env::temp_dir().join(format!("susi-bert-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), b"cached config").unwrap();
        assert_eq!(
            ensure_cached_file(&dir, "http://127.0.0.1:1", "config.json").unwrap(),
            b"cached config"
        );
        std::fs::remove_file(dir.join("config.json")).unwrap();
        std::fs::remove_dir(dir).unwrap();
        assert!(base_url_from(None).contains("5c38ec7c405ec4b44b94cc5a9bb96e735b38267a"));
    }

    /// Explicit qualification downloads public MIT-licensed weights into a
    /// caller-supplied isolated cache; ordinary unit tests never use the network.
    #[test]
    #[ignore = "requires explicit SUSI_EMBEDDING_TEST_CACHE and model provisioning"]
    fn real_bge_model_preserves_semantic_order_batching_and_norm() {
        let cache = std::env::var("SUSI_EMBEDDING_TEST_CACHE").expect("isolated cache required");
        let model = Embedder::load(&PathBuf::from(cache), DEFAULT_MODEL_BASE_URL).unwrap();
        let texts = vec![
            "A dog is playing in the park.".into(),
            "A puppy plays outdoors.".into(),
            "A database transaction was committed.".into(),
        ];
        let vectors = model.embed(&texts).unwrap();
        assert_eq!(vectors.len(), 3);
        for vector in &vectors {
            assert_eq!(vector.len(), EMBEDDING_DIM);
            assert!((vector.iter().map(|value| value * value).sum::<f32>() - 1.).abs() < 1e-4);
        }
        let similarity = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(similarity(&vectors[0], &vectors[1]) > similarity(&vectors[0], &vectors[2]) + 0.1);
        let single = model.embed(&texts[..1]).unwrap();
        assert!(similarity(&vectors[0], &single[0]) > 0.9999);
    }
}
