#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-vendor-fastembed
//!
//! The one crate that links fastembed. [`embed`] runs texts through one
//! process-wide `TextEmbedding` (`Xenova/bge-small-en-v1.5`): the ONNX session
//! load is the expensive part, so it happens once per process and every caller —
//! the `susi-fastembed` capability provider and the semantic index — shares
//! the warm model. A failed load is cached, so a host that cannot load the
//! embedder refuses fast instead of retrying per call.
//!
//! Model artifacts are cached on disk and fetched via `susi-http-transport`
//! so `fastembed` runs without its `hf-hub` feature (eliminating the transitive
//! `option-ext` MPL-2.0 dependency).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use susi_error::{EaiError, EaiResult};

const DEFAULT_MODEL_SUBDIR: &str = "Xenova--bge-small-en-v1.5";
const DEFAULT_MODEL_BASE_URL: &str = "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main";
const MAX_MODEL_FILE_BYTES: u64 = 256 * 1024 * 1024;
const DOWNLOAD_TIMEOUT_SECS: u64 = 120;
const MAX_REDIRECTS: u32 = 5;

type ModelCell = OnceLock<Result<Mutex<fastembed::TextEmbedding>, String>>;

fn model_cell() -> &'static ModelCell {
    static MODEL: ModelCell = OnceLock::new();
    &MODEL
}

fn cache_dir_from(env_override: Option<&str>) -> PathBuf {
    let root = env_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(".fastembed_cache");
    PathBuf::from(root).join(DEFAULT_MODEL_SUBDIR)
}

fn base_url_from(env_override: Option<&str>) -> String {
    env_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL_BASE_URL)
        .trim_end_matches('/')
        .to_string()
}

fn ensure_cached_file(cache_dir: &Path, base_url: &str, rel_path: &str) -> Result<Vec<u8>, String> {
    let dest = cache_dir.join(rel_path);
    if let Ok(bytes) = std::fs::read(&dest) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create cache dir {}: {e}", parent.display()))?;
    }
    let url = format!("{}/{rel_path}", base_url.trim_end_matches('/'));
    let resp =
        susi_http_transport::http_call("GET", &url, &[], DOWNLOAD_TIMEOUT_SECS, MAX_REDIRECTS)
            .map_err(|e| format!("failed to fetch {url}: {e}"))?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("fetching {url} returned HTTP {}", resp.status));
    }
    let bytes = resp
        .into_bytes(MAX_MODEL_FILE_BYTES)
        .map_err(|e| format!("reading {url} failed: {e}"))?;
    if bytes.is_empty() {
        return Err(format!("fetched empty artifact from {url}"));
    }
    let _ = std::fs::write(&dest, &bytes);
    Ok(bytes)
}

fn load_user_defined_model(
    cache_dir: &Path,
    base_url: &str,
) -> Result<fastembed::UserDefinedEmbeddingModel, String> {
    let onnx_file = ensure_cached_file(cache_dir, base_url, "onnx/model.onnx")?;
    let tokenizer_files = fastembed::TokenizerFiles {
        tokenizer_file: ensure_cached_file(cache_dir, base_url, "tokenizer.json")?,
        config_file: ensure_cached_file(cache_dir, base_url, "config.json")?,
        special_tokens_map_file: ensure_cached_file(
            cache_dir,
            base_url,
            "special_tokens_map.json",
        )?,
        tokenizer_config_file: ensure_cached_file(cache_dir, base_url, "tokenizer_config.json")?,
    };
    Ok(
        fastembed::UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files)
            .with_pooling(fastembed::Pooling::Cls),
    )
}

fn init_default_embedder() -> Result<fastembed::TextEmbedding, String> {
    let cache_env = std::env::var("FASTEMBED_CACHE_DIR").ok();
    let url_env = std::env::var("SUSI_FASTEMBED_MODEL_URL").ok();
    let cache_dir = cache_dir_from(cache_env.as_deref());
    let base_url = base_url_from(url_env.as_deref());
    let model = load_user_defined_model(&cache_dir, &base_url)?;
    fastembed::TextEmbedding::try_new_from_user_defined(model, Default::default())
        .map_err(|e| e.to_string())
}

fn model() -> EaiResult<&'static Mutex<fastembed::TextEmbedding>> {
    model_cell()
        .get_or_init(|| {
            init_default_embedder()
                .map(Mutex::new)
                .map_err(|e| format!("embedder unavailable: {e}"))
        })
        .as_ref()
        .map_err(|e| EaiError::inference(e.clone()))
}

/// True once a model load was attempted and failed (the failure is
/// cached for the process). `false` before the first attempt: loading is
/// lazy, so "not yet loaded" is not "unhealthy".
#[must_use]
pub fn load_failed() -> bool {
    model_cell().get().is_some_and(Result::is_err)
}

/// Embed each text; one vector per input, in order.
///
/// # Errors
/// The model cannot load (cached), or inference fails.
pub fn embed(texts: Vec<String>) -> EaiResult<Vec<Vec<f32>>> {
    let mut guard = model()?.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .embed(texts, None)
        .map_err(|e| EaiError::inference(format!("embed failed: {e}")))
}

/// Embed one text.
///
/// # Errors
/// See [`embed`]; also when the model returns no vector.
pub fn embed_one(text: &str) -> EaiResult<Vec<f32>> {
    embed(vec![text.to_string()])?
        .pop()
        .ok_or_else(|| EaiError::inference("embedder returned no vector"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_failed_is_false_before_any_load_attempt() {
        assert!(!super::load_failed());
    }

    #[test]
    fn cache_helpers_and_user_defined_model_bundle_work_offline() {
        assert_eq!(
            cache_dir_from(None),
            PathBuf::from(".fastembed_cache").join(DEFAULT_MODEL_SUBDIR)
        );
        assert_eq!(
            cache_dir_from(Some(" /tmp/custom-fastembed ")),
            PathBuf::from("/tmp/custom-fastembed").join(DEFAULT_MODEL_SUBDIR)
        );
        assert_eq!(base_url_from(None), DEFAULT_MODEL_BASE_URL);
        assert_eq!(
            base_url_from(Some("https://example.invalid/model/")),
            "https://example.invalid/model"
        );

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_dir = std::env::temp_dir().join(format!(
            "susi-fastembed-test-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(temp_dir.join("onnx")).expect("create temp onnx dir");
        for (rel, content) in [
            ("onnx/model.onnx", &b"fake-onnx"[..]),
            ("tokenizer.json", &b"{}"[..]),
            ("config.json", &b"{}"[..]),
            ("special_tokens_map.json", &b"{}"[..]),
            ("tokenizer_config.json", &b"{}"[..]),
        ] {
            std::fs::write(temp_dir.join(rel), content).expect("seed cached model file");
        }

        let loaded = load_user_defined_model(&temp_dir, "http://127.0.0.1:1")
            .expect("cached files should load without network");
        assert_eq!(loaded.onnx_file, b"fake-onnx");
        assert_eq!(loaded.pooling, Some(fastembed::Pooling::Cls));

        let miss_err = ensure_cached_file(&temp_dir, "http://127.0.0.1:1", "missing/artifact.bin")
            .expect_err("cache miss against closed loopback port must fail");
        assert!(miss_err.contains("failed to fetch"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
