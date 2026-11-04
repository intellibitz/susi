#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Rust-native semantic embeddings, retaining the historical facade name.
//!
//! BAAI/bge-small-en-v1.5 runs through Candle's BERT implementation: the
//! pretrained CLS token is L2 normalized, producing 384-dimensional vectors.
//! No ONNX Runtime, Python, native model runtime or synthetic hash fallback is
//! involved. The MIT-licensed model revision is pinned. Core never downloads
//! it by itself (CEO decision 2026-11-04, "No, ask first"): `embed` loads only
//! verified cached artifacts and otherwise fails with a typed
//! `[NO_EMBEDDING_MODEL]` error naming [`INSTALL_COMMAND`]; that explicit
//! command calls [`install_on_request`], which fetches the artifacts through
//! the existing bounded HTTP transport. CPU is the portable default. Existing
//! public `embed`, `embed_one`, and `load_failed` APIs remain.

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
const ARTIFACTS: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];

/// The explicit command that downloads the embedding model.
pub const INSTALL_COMMAND: &str = "susi os manage model install embeddings";
/// The explicit source argument `INSTALL_COMMAND` passes.
pub const INSTALL_SOURCE: &str = "embeddings";
/// Leading tag of the not-installed error, for callers that carry it as text.
pub const NOT_INSTALLED_TAG: &str = "[NO_EMBEDDING_MODEL]";

/// The typed error a lazy load returns when the model is not cached.
fn not_installed_message() -> String {
    format!(
        "{NOT_INSTALLED_TAG} The BAAI/bge-small-en-v1.5 embedding model is not installed, \
         and Susi does not download a model by itself. Run `{INSTALL_COMMAND}` to download \
         it. Semantic search needs it; chat and keyword search do not."
    )
}

/// Cache roots a lazy load searches: `FASTEMBED_CACHE_DIR` when set,
/// otherwise the working directory's `.fastembed_cache` (the daemon's
/// working directory is the substrate home) and the substrate home's.
fn cache_roots(env_override: Option<&str>) -> Vec<PathBuf> {
    match env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(root) => vec![PathBuf::from(root)],
        None => vec![PathBuf::from(".fastembed_cache"), install_root_from(None)],
    }
}

/// Where [`install_on_request`] writes: `FASTEMBED_CACHE_DIR` when set,
/// otherwise the substrate home's `.fastembed_cache`, which the daemon reads.
fn install_root_from(env_override: Option<&str>) -> PathBuf {
    env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| susi_paths::SusiDirs::substrate_home().join(".fastembed_cache"))
}

/// The first model directory under `roots` holding every artifact.
fn installed_dir(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join(DEFAULT_MODEL_SUBDIR))
        .find(|dir| {
            ARTIFACTS.iter().all(|name| {
                std::fs::metadata(dir.join(name)).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
            })
        })
}

struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
}

type ModelCell = OnceLock<Result<Mutex<Embedder>, String>>;

fn model_cell() -> &'static ModelCell {
    static MODEL: ModelCell = OnceLock::new();
    &MODEL
}

fn base_url_from(env_override: Option<&str>) -> String {
    env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MODEL_BASE_URL)
        .trim_end_matches('/')
        .to_owned()
}

fn verify_pinned_artifact(name: &str, bytes: &[u8]) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let expected = match name {
        "model.safetensors" => "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad",
        "config.json" => "094f8e891b932f2000c92cfc663bac4c62069f5d8af5b5278c4306aef3084750",
        "tokenizer.json" => "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        _ => return Err("Unrecognized BGE artifact".into()),
    };
    if Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        != expected
    {
        return Err(format!("[MODEL LICENSE] {name} does not match the audited MIT-licensed BGE revision; cache contents are preserved for inspection"));
    }
    Ok(())
}

/// A verified cached artifact, `None` when it is not cached. Never touches
/// the network.
fn read_cached_file(cache: &Path, base: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
    if base.trim_end_matches('/') != DEFAULT_MODEL_BASE_URL {
        return Err("[MODEL LICENSE] Unverified embedding model override refused; use the pinned MIT-licensed BGE model. Custom cache locations remain supported.".into());
    }
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
            if bytes.is_empty() {
                return Ok(None);
            }
            verify_pinned_artifact(name, &bytes)?;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("opening {}: {error}", path.display())),
    }
}

/// A verified cached artifact for a lazy load: never downloads, and a
/// missing artifact is the typed not-installed error.
fn cached_only(cache: &Path, base: &str, name: &str) -> Result<Vec<u8>, String> {
    read_cached_file(cache, base, name)?.ok_or_else(not_installed_message)
}

/// Download (or re-verify) the pinned embedding model into the install
/// root. Only the user's explicit [`INSTALL_COMMAND`] calls this; core never
/// calls it on its own.
pub fn install_on_request() -> EaiResult<String> {
    let base = base_url_from(std::env::var("SUSI_FASTEMBED_MODEL_URL").ok().as_deref());
    let dir = install_root_from(std::env::var("FASTEMBED_CACHE_DIR").ok().as_deref())
        .join(DEFAULT_MODEL_SUBDIR);
    for name in ARTIFACTS {
        ensure_cached_file(&dir, &base, name)
            .map_err(|error| EaiError::inference(format!("embedding model install: {error}")))?;
    }
    Ok(format!(
        "Installed and verified the BAAI/bge-small-en-v1.5 embedding model in {}",
        dir.display()
    ))
}

fn ensure_cached_file(cache: &Path, base: &str, name: &str) -> Result<Vec<u8>, String> {
    ensure_cached_file_with_policy(cache, base, name, &susi_core::mac_policy::egress_permitted)
}
fn ensure_cached_file_with_policy(
    cache: &Path,
    base: &str,
    name: &str,
    egress_allowed: &dyn Fn(&str) -> bool,
) -> Result<Vec<u8>, String> {
    if let Some(bytes) = read_cached_file(cache, base, name)? {
        return Ok(bytes);
    }
    let path = cache.join(name);
    let url = format!("{base}/{name}");
    if !egress_allowed(&url) {
        return Err("[PRIVACY] Embedding download denied by network-egress policy; verified cached artifacts remain available".into());
    }
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
    verify_pinned_artifact(name, &bytes)?;
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
        let config: Config = serde_json::from_slice(&cached_only(cache, base, "config.json")?)
            .map_err(|e| format!("invalid BERT config: {e}"))?;
        if config.hidden_size != EMBEDDING_DIM || config.max_position_embeddings < 2 {
            return Err("expected the 384-dimensional BGE small model".into());
        }
        let mut tokenizer = Tokenizer::from_bytes(cached_only(cache, base, "tokenizer.json")?)
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
        let weights = cached_only(cache, base, "model.safetensors")?;
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
    // Not installed is not a load failure: leave the cell empty, so the
    // model loads once the user runs `INSTALL_COMMAND`, without a restart.
    let dir = if model_cell().get().is_none() {
        let roots = cache_roots(std::env::var("FASTEMBED_CACHE_DIR").ok().as_deref());
        Some(installed_dir(&roots).ok_or_else(|| EaiError::inference(not_installed_message()))?)
    } else {
        None
    };
    model_cell()
        .get_or_init(|| {
            let dir = dir.ok_or_else(not_installed_message)?;
            let base = base_url_from(std::env::var("SUSI_FASTEMBED_MODEL_URL").ok().as_deref());
            Embedder::load(&dir, &base).map(Mutex::new)
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

    /// An empty cache is the typed not-installed error naming the explicit
    /// install command; a lazy load never reaches the network for it.
    #[test]
    fn a_missing_model_is_not_installed_and_never_downloaded_lazily() {
        let root = std::env::temp_dir().join(format!("susi-bge-empty-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(installed_dir(std::slice::from_ref(&root)), None);
        let dir = root.join(DEFAULT_MODEL_SUBDIR);
        for name in ARTIFACTS {
            let error = cached_only(&dir, DEFAULT_MODEL_BASE_URL, name).unwrap_err();
            assert!(error.starts_with(NOT_INSTALLED_TAG), "{error}");
            assert!(error.contains(INSTALL_COMMAND), "{error}");
        }
        assert!(!dir.exists(), "a lazy load created the model cache");
        let error = Embedder::load(&dir, DEFAULT_MODEL_BASE_URL)
            .err()
            .expect("an empty cache cannot load");
        assert!(error.starts_with(NOT_INSTALLED_TAG), "{error}");
        std::fs::remove_dir_all(&root).unwrap();

        let override_roots = cache_roots(Some("C:/custom"));
        assert_eq!(override_roots, vec![PathBuf::from("C:/custom")]);
        let default_roots = cache_roots(None);
        assert_eq!(default_roots[0], PathBuf::from(".fastembed_cache"));
        assert_eq!(default_roots[1], install_root_from(None));
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
    fn altered_cache_and_unverified_origins_are_rejected() {
        let dir = std::env::temp_dir().join(format!("susi-bert-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(ensure_cached_file_with_policy(
            &dir,
            DEFAULT_MODEL_BASE_URL,
            "config.json",
            &|_| false
        )
        .unwrap_err()
        .contains("PRIVACY"));
        std::fs::write(dir.join("config.json"), b"cached config").unwrap();
        assert!(ensure_cached_file(&dir, "http://127.0.0.1:1", "config.json").is_err());
        assert!(
            ensure_cached_file(&dir, DEFAULT_MODEL_BASE_URL, "config.json")
                .unwrap_err()
                .contains("does not match")
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
        ensure_cached_file_with_policy(
            &PathBuf::from(&cache),
            DEFAULT_MODEL_BASE_URL,
            "config.json",
            &|_| false,
        )
        .unwrap();
        let model = Embedder::load(&PathBuf::from(cache), DEFAULT_MODEL_BASE_URL).unwrap();
        let texts = vec![
            "A dog is playing in the park.".into(),
            "A puppy plays outdoors.".into(),
            "A database transaction was committed.".into(),
        ];
        let vectors = model.embed(&texts).unwrap();
        assert_eq!(vectors.len(), 3);
        // Captured from the previous ONNX backend using the same pretrained
        // BGE model and first input. Keep a compact numerical regression anchor.
        let onnx_prefix = [
            0.055796586,
            -0.075012155,
            0.041990824,
            0.026629992,
            0.008970343,
            0.046833955,
            0.05523868,
            -0.01734583,
        ];
        for (actual, expected) in vectors[0].iter().zip(onnx_prefix) {
            assert!((actual - expected).abs() < 1e-4);
        }
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
