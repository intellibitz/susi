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

/// The explicit embedding-model install, re-exported so the CLI reaches it
/// through this crate instead of linking the embedding engine directly.
pub use susi_vendor_fastembed::{install_on_request, INSTALL_SOURCE};

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn local_embed_provider_reports_canonical_name_and_downcasts_via_as_any() {
        let provider = LocalEmbedProvider;
        assert_eq!(provider.name(), "susi-fastembed");
        assert!(provider
            .as_any()
            .downcast_ref::<LocalEmbedProvider>()
            .is_some());
    }

    #[test]
    fn local_embed_provider_generate_rejects_chat_prompts_as_embedding_only() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(LocalEmbedProvider.generate("hello"))
            .unwrap_err();
        assert!(err.to_string().contains("embedding-only provider"));
    }

    #[test]
    fn local_embed_provider_health_check_returns_boolean_status() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let healthy = rt.block_on(LocalEmbedProvider.is_healthy()).unwrap();
        assert_eq!(healthy, !susi_vendor_fastembed::load_failed());
    }

    #[test]
    fn register_local_embed_provider_is_idempotent_and_exposes_install_source() {
        register_local_embed_provider();
        register_local_embed_provider();
        let registry = crate::susi_core::registry::CapabilityRegistry::global();
        assert!(registry.get_provider("susi-fastembed").is_some());
        assert_eq!(INSTALL_SOURCE, "embeddings");
    }

    #[test]
    fn registered_local_embed_provider_rejects_generate_through_capability_registry() {
        register_local_embed_provider();
        let registry = crate::susi_core::registry::CapabilityRegistry::global();
        let provider = registry
            .get_provider("susi-fastembed")
            .expect("registered susi-fastembed provider");
        assert_eq!(provider.name(), "susi-fastembed");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(provider.generate("chat prompt")).unwrap_err();
        assert!(err.to_string().contains("embedding-only provider"));
    }
}
