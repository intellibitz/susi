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

fn model() -> EaiResult<&'static Mutex<fastembed::TextEmbedding>> {
    static MODEL: OnceLock<Result<Mutex<fastembed::TextEmbedding>, String>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            fastembed::TextEmbedding::try_new(Default::default())
                .map(Mutex::new)
                .map_err(|e| format!("embedder unavailable: {e}"))
        })
        .as_ref()
        .map_err(|e| EaiError::inference(e.clone()))
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
