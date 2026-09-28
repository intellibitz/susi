#![forbid(unsafe_code)]

//! # susi-vendor-fastembed
//!
//! The one crate that links fastembed. [`embed`] runs texts through one
//! process-wide `TextEmbedding` (default model): the ONNX session load is
//! the expensive part, so it happens once per process and every caller —
//! the `susi-fastembed` capability provider and the semantic index — shares
//! the warm model. A failed load is cached, so a host that cannot load the
//! embedder refuses fast instead of retrying per call.

use std::sync::{Mutex, OnceLock};
use susi_error::{EaiError, EaiResult};

type ModelCell = OnceLock<Result<Mutex<fastembed::TextEmbedding>, String>>;

fn model_cell() -> &'static ModelCell {
    static MODEL: ModelCell = OnceLock::new();
    &MODEL
}

fn model() -> EaiResult<&'static Mutex<fastembed::TextEmbedding>> {
    model_cell()
        .get_or_init(|| {
            fastembed::TextEmbedding::try_new(Default::default())
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
    #[test]
    fn load_failed_is_false_before_any_load_attempt() {
        assert!(!super::load_failed());
    }
}
