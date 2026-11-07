//! Native inference engines and the process-wide engine registry.

use super::GemiEngine;
use crate::susi_core::registry::DynamicServiceRegistry;
use crate::susi_error::{EaiError, EaiResult};
use std::sync::{Arc, OnceLock};

pub trait NativeInferenceEngine: Send + Sync {
    fn name(&self) -> String;
    fn run_inference(&self, prompt: &str) -> EaiResult<String>;
    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String>;
}

/// Legacy registry/API spelling. This delegates to the in-process Rust Candle engine.
pub struct LlamaCppEngine;

impl NativeInferenceEngine for LlamaCppEngine {
    fn name(&self) -> String {
        "CandleGgufEngine".to_string()
    }
    fn run_inference(&self, prompt: &str) -> EaiResult<String> {
        // Native Priority: Use the hardened SusiGgufEngine directly
        SusiGgufEngine.run_inference(prompt)
    }
    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String> {
        SusiGgufEngine.run_inference_stream(prompt, callback, selected_model)
    }
}

pub struct SusiGgufEngine;

impl NativeInferenceEngine for SusiGgufEngine {
    fn name(&self) -> String {
        "CandleGgufEngine".to_string()
    }

    fn run_inference(&self, prompt: &str) -> EaiResult<String> {
        self.run_inference_stream(prompt, &|_| {}, None)
    }

    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String> {
        let task_handle = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("neural_inference", prompt);

        // Test seam: skip the native model load (31B load timeouts on CI and
        // hardware-limited hosts). It reports that no inference ran — a
        // typed failure the cascade handles like any unavailable engine —
        // rather than returning fabricated text as if a model answered.
        if std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true" || cfg!(test) {
            let reason = "native inference disabled (SUSI_TEST_MOCK_INFERENCE / test build)";
            task_handle.mark_failed(reason);
            return Err(EaiError::inference(reason));
        }

        // Every early-? return must mark the task failed — otherwise the
        // record lingers as a zombie until the watchdog's idle lease expires
        // and logs a spurious LEASE_EXPIRED cancellation.
        let result =
            self.run_inference_stream_inner(prompt, callback, selected_model, &task_handle);
        if let Err(e) = &result {
            task_handle.mark_failed(&e.to_string());
        }
        result
    }
}

impl SusiGgufEngine {
    fn run_inference_stream_inner(
        &self,
        _prompt: &str,
        _callback: &dyn Fn(String),
        _selected_model: Option<&str>,
        _task_handle: &Arc<crate::susi_core::task_manager::TaskHandle>,
    ) -> EaiResult<String> {
        Err(EaiError::inference(
            "built-in local model engine is not in this core; rebuild with --features local-engine",
        ))
    }
}

pub struct SusiFederatedEngine;

impl NativeInferenceEngine for SusiFederatedEngine {
    fn name(&self) -> String {
        "SusiFederatedEngine".to_string()
    }

    fn run_inference(&self, prompt: &str) -> EaiResult<String> {
        self.run_inference_stream(prompt, &|_| {}, None)
    }

    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        _selected_model: Option<&str>,
    ) -> EaiResult<String> {
        tracing::debug!("[SUSI Federated Router] Requesting remote consensus quorum...");

        // Prefer CapabilityRegistry cloud/local HTTP providers (keys + protocols
        // already resolved by zero-config / config registration).
        crate::engines::http_provider::register_configured_cloud_endpoints(
            crate::susi_core::registry::CapabilityRegistry::global(),
        );
        let (text, mut ladder) =
            GemiEngine::try_discovered_providers(prompt, None, None, callback, &|_| {});
        if ladder.step_downs() > 0 {
            callback(format!("[SUSI ROUTING] {}\n", ladder.summary()));
        }
        ladder.persist();
        if let Some(text) = text {
            return Ok(text);
        }

        // Fallback: first config endpoint that has a resolvable API key (or is local).
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let endpoints = cfg.inference_endpoints();
        let endpoint = endpoints
            .endpoints
            .iter()
            .find(|e| {
                let key = crate::engines::http_provider::HttpProvider::resolve_api_key(
                    &e.api_key_env,
                    &e.name,
                );
                !key.is_empty()
                    || e.api_base.contains("localhost")
                    || e.api_base.contains("127.0.0.1")
            })
            .or_else(|| endpoints.endpoints.first())
            .ok_or_else(|| {
                crate::susi_error::EaiError::inference(
                    "No active federated endpoints provisioned in config.default.json.",
                )
            })?;

        let protocol =
            crate::engines::http_provider::InferenceProtocol::from_config(&endpoint.protocol_type);
        let api_key = crate::engines::http_provider::HttpProvider::resolve_api_key(
            &endpoint.api_key_env,
            &endpoint.name,
        );
        let model = if endpoint.model.is_empty() {
            "gpt-4o-mini".to_string()
        } else {
            endpoint.model.clone()
        };

        tracing::debug!(
            "[SUSI Federated Router] Edge Delegation Active: {} ({}) model={} protocol={:?}",
            endpoint.name,
            endpoint.api_base,
            model,
            protocol
        );

        let provider = crate::engines::http_provider::HttpProvider {
            name: format!("federated-{}", endpoint.name.to_ascii_lowercase()),
            api_base: endpoint.api_base.clone(),
            model,
            protocol,
            api_key,
        };

        let runtime = GemiEngine::provider_runtime().ok_or_else(|| {
            crate::susi_error::EaiError::inference("Failed to start federated inference runtime")
        })?;
        match runtime.block_on(crate::susi_core::Provider::generate(&provider, prompt)) {
            Ok(content) if !content.trim().is_empty() => {
                callback(content.clone());
                Ok(content)
            }
            Ok(_) => Err(crate::susi_error::EaiError::inference(
                "Federated swarm endpoint returned empty consensus payload.",
            )),
            Err(e) => Err(crate::susi_error::EaiError::inference(format!(
                "Federated edge connection refused: {}",
                e
            ))),
        }
    }
}

pub fn engine_registry() -> &'static DynamicServiceRegistry {
    static REGISTRY: OnceLock<DynamicServiceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let registry = DynamicServiceRegistry::new();
        registry.register_factory("susi-federated", || {
            Arc::new(Arc::new(SusiFederatedEngine) as Arc<dyn NativeInferenceEngine>)
        });
        registry.register_factory("cloud", || {
            Arc::new(Arc::new(SusiFederatedEngine) as Arc<dyn NativeInferenceEngine>)
        });
        registry.register_factory("candle", || {
            Arc::new(Arc::new(SusiGgufEngine) as Arc<dyn NativeInferenceEngine>)
        });
        registry.register_factory("llamacpp", || {
            Arc::new(Arc::new(LlamaCppEngine) as Arc<dyn NativeInferenceEngine>)
        });
        registry
    })
}

#[cfg(test)]
mod candle_routing_tests {
    use super::*;
    #[test]
    fn canonical_and_legacy_registry_names_resolve_to_rust_candle() {
        for key in ["candle", "llamacpp"] {
            let engine = engine_registry()
                .instantiate::<Arc<dyn NativeInferenceEngine>>(key)
                .unwrap();
            assert_eq!(engine.name(), "CandleGgufEngine");
        }
    }

    #[test]
    fn federated_and_cloud_registry_keys_resolve_to_federated_engine() {
        for key in ["susi-federated", "cloud"] {
            let engine = engine_registry()
                .instantiate::<Arc<dyn NativeInferenceEngine>>(key)
                .unwrap();
            assert_eq!(engine.name(), "SusiFederatedEngine");
        }
        assert!(engine_registry()
            .instantiate::<Arc<dyn NativeInferenceEngine>>("nonexistent-engine")
            .is_none());
    }

    #[test]
    fn susi_gguf_engine_fails_closed_under_test_seam() {
        let err = SusiGgufEngine
            .run_inference("ping")
            .expect_err("test build must short-circuit native inference");
        assert!(err.to_string().contains("native inference disabled"));
    }

    #[test]
    fn llama_cpp_engine_delegates_to_susi_gguf_test_seam() {
        let err = LlamaCppEngine
            .run_inference("ping")
            .expect_err("legacy wrapper must delegate to SusiGgufEngine test seam");
        assert!(err.to_string().contains("native inference disabled"));
    }

    #[test]
    fn engine_implementations_report_canonical_names() {
        assert_eq!(SusiGgufEngine.name(), "CandleGgufEngine");
        assert_eq!(LlamaCppEngine.name(), "CandleGgufEngine");
        assert_eq!(SusiFederatedEngine.name(), "SusiFederatedEngine");
    }
}
