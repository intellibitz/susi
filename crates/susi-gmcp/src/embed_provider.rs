//! Local embedding provider contract (`susi-fastembed`).
//!
//! The heavy local BGE Candle embedding engine was migrated to `susi-commercial-apps`
//! (`susi-gpu-fabric`) in Lean-Core Rung 1. Core retains the capability registry
//! contract (`LocalEmbedProvider`, `register_local_embed_provider`, `INSTALL_SOURCE`,
//! `install_on_request`, `embed`, `embed_one`, `load_failed`).

use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_error::{EaiError, EaiResult};
use std::any::Any;

/// The explicit command that downloads the embedding model.
pub const INSTALL_COMMAND: &str = "susi os manage model install embeddings";
/// The explicit source argument `INSTALL_COMMAND` passes.
pub const INSTALL_SOURCE: &str = "embeddings";
/// Leading tag of the not-installed error, for callers that carry it as text.
pub const NOT_INSTALLED_TAG: &str = "[NO_EMBEDDING_MODEL]";

#[must_use]
pub fn load_failed() -> bool {
    false
}

pub fn install_on_request() -> EaiResult<String> {
    Err(EaiError::inference(format!(
        "{NOT_INSTALLED_TAG} Local BGE embedding engine moved to susi-commercial-apps (susi-gpu-fabric)."
    )))
}

pub fn embed(texts: Vec<String>) -> EaiResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    Err(EaiError::inference(format!(
        "{NOT_INSTALLED_TAG} The BAAI/bge-small-en-v1.5 embedding model is not installed in core; run `{INSTALL_COMMAND}` or attach the susi-gpu-fabric plug."
    )))
}

pub fn embed_one(text: &str) -> EaiResult<Vec<f32>> {
    embed(vec![text.to_owned()])?
        .pop()
        .ok_or_else(|| EaiError::inference("embedder returned no vector"))
}

pub struct LocalEmbedProvider;

impl Provider for LocalEmbedProvider {
    fn name(&self) -> &str {
        "susi-fastembed"
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async { Ok(!load_failed()) })
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
        Box::pin(async move { embed_one(&text) })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Self-register into this copy's `CapabilityRegistry` — idempotent.
pub fn register_local_embed_provider() {
    let registry = crate::susi_core::registry::CapabilityRegistry::global();
    if registry.get_provider("susi-fastembed").is_none() {
        registry.register_provider(LocalEmbedProvider);
    }
}

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
        assert_eq!(healthy, !load_failed());
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
