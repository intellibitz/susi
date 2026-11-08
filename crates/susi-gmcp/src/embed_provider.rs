//! Local embedding provider contract (`susi-fastembed`).
//!
//! The heavy local BGE Candle embedding engine was migrated to `susi-commercial-apps`
//! (`susi-gpu-fabric`) in Lean-Core Rung 1. Core retains the capability registry
//! contract (`LocalEmbedProvider`, `register_local_embed_provider`, `INSTALL_SOURCE`,
//! `install_on_request`, `embed`, `embed_one`, `load_failed`) and serves it through
//! the plane bus: when a process on this substrate's bus rendezvous publishes
//! [`TOPIC_EMBED_TEXTS`], every call here is answered by it; otherwise the
//! not-installed error names the install command.

use crate::susi_core::plane_bus::PlaneBus;
use crate::susi_core::provider::{BoxFuture, Provider};
use crate::susi_error::{EaiError, EaiResult};
use serde_json::{json, Value};
use std::any::Any;

/// The explicit command that downloads the embedding model.
pub const INSTALL_COMMAND: &str = "susi os manage model install embeddings";
/// The explicit source argument `INSTALL_COMMAND` passes.
pub const INSTALL_SOURCE: &str = "embeddings";
/// Leading tag of the not-installed error, for callers that carry it as text.
pub const NOT_INSTALLED_TAG: &str = "[NO_EMBEDDING_MODEL]";
/// Plane-bus topic a plug serves to embed texts: `{"texts": [..]}` in,
/// `{"vectors": [[f32]]}` out.
pub const TOPIC_EMBED_TEXTS: &str = "gpu_fabric.embed.texts";
/// Plane-bus topic a plug serves for the explicit model install: `{}` in,
/// `{"message": ".."}` out.
pub const TOPIC_EMBED_INSTALL: &str = "gpu_fabric.embed.install";

/// Whether a process on this substrate's bus serves the embedding topics.
#[must_use]
pub fn plug_wired() -> bool {
    PlaneBus::global().is_wired(TOPIC_EMBED_TEXTS)
}

#[must_use]
pub fn load_failed() -> bool {
    false
}

pub fn install_on_request() -> EaiResult<String> {
    if plug_wired() {
        let reply = PlaneBus::global()
            .request(TOPIC_EMBED_INSTALL, json!({}))
            .map_err(EaiError::inference)?;
        return Ok(reply
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("embedding model installed by the attached plug")
            .to_string());
    }
    Err(EaiError::inference(format!(
        "{NOT_INSTALLED_TAG} Local BGE embedding engine lives in susi-commercial-apps (susi-gpu-fabric); start the susi-plug-host beside this core."
    )))
}

pub fn embed(texts: Vec<String>) -> EaiResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    if plug_wired() {
        let count = texts.len();
        let reply = PlaneBus::global()
            .request(TOPIC_EMBED_TEXTS, json!({ "texts": texts }))
            .map_err(EaiError::inference)?;
        let vectors: Vec<Vec<f32>> = reply
            .get("vectors")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| EaiError::inference(format!("embedding plug reply: {e}")))?
            .ok_or_else(|| EaiError::inference("embedding plug reply has no `vectors`"))?;
        if vectors.len() != count {
            return Err(EaiError::inference(format!(
                "embedding plug returned {} vectors for {count} texts",
                vectors.len()
            )));
        }
        return Ok(vectors);
    }
    Err(EaiError::inference(format!(
        "{NOT_INSTALLED_TAG} The BAAI/bge-small-en-v1.5 embedding model is not installed in core; run `{INSTALL_COMMAND}` or start the susi-plug-host (susi-gpu-fabric) beside this core."
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

    struct FakeEmbedPlug;
    impl crate::susi_core::plane_bus::PlaneHandler for FakeEmbedPlug {
        fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
            match topic {
                TOPIC_EMBED_TEXTS => {
                    let n = payload["texts"].as_array().map_or(0, Vec::len);
                    Ok(json!({ "vectors": vec![vec![0.5_f32, 0.25]; n] }))
                }
                TOPIC_EMBED_INSTALL => Ok(json!({ "message": "already installed" })),
                other => Err(format!("fake plug: {other}")),
            }
        }
    }

    #[test]
    fn a_wired_plug_answers_embed_and_install_through_the_bus() {
        PlaneBus::global().register(TOPIC_EMBED_TEXTS, std::sync::Arc::new(FakeEmbedPlug));
        PlaneBus::global().register(TOPIC_EMBED_INSTALL, std::sync::Arc::new(FakeEmbedPlug));
        assert!(plug_wired());
        let vectors = embed(vec!["a".into(), "b".into()]).unwrap();
        assert_eq!(vectors, vec![vec![0.5, 0.25], vec![0.5, 0.25]]);
        assert_eq!(embed_one("a").unwrap(), vec![0.5, 0.25]);
        assert_eq!(install_on_request().unwrap(), "already installed");
        assert!(embed(Vec::new()).unwrap().is_empty());
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
