//! Local embedding provider backed by `fastembed` — the same embedder
//! `SemanticIndex` uses, exposed through the capability registry so
//! `gemi.infer.embed` (and `/v1/embeddings`) works with zero external
//! dependencies. Registers into this copy's registry; the IPC rendezvous
//! serves it to every other vendored `susi_core` copy in the process.
//!
//! The provider is embedding-only: `generate` fails loudly rather than
//! pretending to be a chat backend, and `cloud_failover_order` never
//! enumerates it (it is neither a remote `HttpProvider` nor `mcp-*`).

use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_error::{EaiError, EaiResult};
use std::any::Any;
use std::sync::{Mutex, OnceLock};

pub struct LocalEmbedProvider;

/// One `TextEmbedding` for the process — model load is the expensive part
/// (ONNX session init), so the first `embed` pays it once and every later
/// call reuses the warm model. A failed load is cached: a machine that
/// cannot load the embedder refuses fast instead of retrying per call.
fn embedder() -> Result<&'static Mutex<fastembed::TextEmbedding>, String> {
    static MODEL: OnceLock<Result<Mutex<fastembed::TextEmbedding>, String>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            fastembed::TextEmbedding::try_new(Default::default())
                .map(Mutex::new)
                .map_err(|e| format!("embedder unavailable: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl Provider for LocalEmbedProvider {
    fn name(&self) -> &str {
        "susi-fastembed"
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(true) })
    }

    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        Box::pin(async {
            Err(EaiError::inference(
                "susi-fastembed is an embedding-only provider",
            ))
        })
    }

    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        let text = text.to_string();
        Box::pin(async move {
            let model = embedder().map_err(EaiError::inference)?;
            let mut guard = model.lock().unwrap_or_else(|e| e.into_inner());
            let mut vectors = guard
                .embed(vec![text], None)
                .map_err(|e| EaiError::inference(format!("embed failed: {e}")))?;
            vectors
                .pop()
                .ok_or_else(|| EaiError::inference("embedder returned no vector"))
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Self-register into this copy's `CapabilityRegistry` — idempotent.
/// Called by the daemon composition root at bootstrap; independent susi
/// binaries that link susi-gmcp can call it the same way.
pub fn register_local_embed_provider() {
    let registry = crate::susi_core::registry::CapabilityRegistry::global();
    if registry.get_provider("susi-fastembed").is_none() {
        registry.register_provider(LocalEmbedProvider);
    }
}
