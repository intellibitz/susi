//! Local embedding provider backed by `fastembed` — the same embedder
//! `SemanticIndex` uses, exposed through the capability registry so
//! `gemi.infer.embed` (and `/v1/embeddings`) works with zero external
//! dependencies. Registers into this process's registry; the IPC rendezvous
//! serves it to every peer process on the same substrate.
//!
//! The provider is embedding-only: `generate` fails loudly rather than
//! pretending to be a chat backend, and `cloud_failover_order` never
//! enumerates it (it is neither a remote `HttpProvider` nor `mcp-*`).

use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_error::{EaiError, EaiResult};
use std::any::Any;

pub struct LocalEmbedProvider;

impl Provider for LocalEmbedProvider {
    fn name(&self) -> &str {
        "susi-fastembed"
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        // Unhealthy once the model load has failed (cached); before the
        // first embed the model is simply not loaded yet.
        Box::pin(async { Ok(!susi_vendor_fastembed::load_failed()) })
    }

    fn generate(&self, _prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        Box::pin(async {
            Err(EaiError::inference(
                "susi-fastembed is an embedding-only provider",
            ))
        })
    }

    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        // One process-wide model, shared with the semantic index
        // (`susi_vendor_fastembed`); the first call pays the ONNX load.
        let text = text.to_string();
        Box::pin(async move { susi_vendor_fastembed::embed_one(&text) })
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
