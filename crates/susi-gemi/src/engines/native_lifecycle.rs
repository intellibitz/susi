//! Native inference engines driven through the shared lifecycle contract
//! (VC-201-042, T-DEEPSEEK-231).
//!
//! `LlamaCppEngine`/`SusiGgufEngine` and `SusiFederatedEngine` are adapted
//! onto `runtime_lifecycle::RuntimeBackend` so the production engine call
//! runs discover → load → ready → infer through the one contract.
//! `InferenceHost` is the adapter's substrate surface: `load` stages
//! weights through `InferenceHost::preload`, `health` probes
//! `InferenceHost::model_is_reusable` (current identity/device/KV, never file
//! presence), and `unload` evicts through `InferenceHost::unload`.
//! `InferenceHost::loaded_models` remains a diagnostic snapshot, not readiness.
//! Verbs a native engine genuinely lacks — external cancel of an
//! unscoped inference task, unload of a federated endpoint — are
//! capability-gated to typed errors, not silent no-ops.

use std::sync::{Arc, Mutex};

use crate::models::runtime_lifecycle::{Capabilities, InferOutcome, Runtime, RuntimeBackend};
use crate::susi_error::{EaiError, EaiResult};

use super::runtime::NativeInferenceEngine;

/// Which kind of engine the backend adapts — the verbs map differently:
/// a native engine's model is a local weight file; a federated engine's
/// "model" is a delegation target and there are no weights to stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Local candle/gguf engine — `load`/`health`/`unload` act on the
    /// `InferenceHost` substrate cache.
    Native,
    /// Delegating engine — `load` records the delegation target and
    /// `health` probes whether a usable endpoint survives cooldown.
    Federated,
}

/// The substrate + routing surface a backend probes. Production uses
/// [`SusiHost`]; tests inject a fake so no weights or endpoints are real.
pub trait EngineHost: Send + Sync {
    /// The model id inference would resolve: the explicit hint when one
    /// resolves to weights, else the request-selected model.
    fn resolve_model(&self, hint: Option<&str>, prompt: &str) -> Option<String>;
    /// Substrate probe — `Ok(true)` when the model's weights are resident
    /// (loaded or mid-generation), `Err(reason)` when they are not.
    fn model_resident(&self, model: &str) -> Result<bool, String>;
    /// Stage the model's weights so the next request is served warm.
    fn preload(&self, model: &str) -> EaiResult<()>;
    /// Evict the model's weights from the substrate cache.
    fn unload(&self, model: &str) -> EaiResult<()>;
    /// Resolvable federated delegation endpoints (keyed or local).
    fn endpoints(&self) -> Vec<String>;
    /// Whether a provider endpoint is inside its post-failure cooldown.
    fn provider_cooled(&self, endpoint: &str) -> bool;
}

/// Production host: `InferenceHost` for weights, `ModelManager` for
/// resolution, config endpoints + routing cooldown for federation.
pub struct SusiHost;

impl EngineHost for SusiHost {
    fn resolve_model(&self, hint: Option<&str>, prompt: &str) -> Option<String> {
        if let Some(id) = hint {
            if crate::models::ModelManager::get_model_path(id).is_some() {
                return Some(id.to_string());
            }
        }
        crate::models::ModelManager::get_selected_model_for_request(prompt)
    }

    fn model_resident(&self, model: &str) -> Result<bool, String> {
        let Some(path) = crate::models::ModelManager::get_model_path(model) else {
            return Err(format!("model '{model}' has no resolvable weights"));
        };

        Ok(crate::engines::runtime::InferenceHost::model_is_reusable(
            &path,
        ))
    }

    fn preload(&self, model: &str) -> EaiResult<()> {
        let path = crate::models::ModelManager::get_model_path(model).ok_or_else(|| {
            EaiError::inference(format!("model '{model}' has no resolvable weights"))
        })?;

        crate::engines::runtime::InferenceHost::preload_resolved(&path).map(|_| ())
    }

    fn unload(&self, model: &str) -> EaiResult<()> {
        let path = crate::models::ModelManager::get_model_path(model).ok_or_else(|| {
            EaiError::inference(format!("model '{model}' has no resolvable weights"))
        })?;

        crate::engines::runtime::InferenceHost::unload_resolved(&path).map(|_| ())
    }

    fn endpoints(&self) -> Vec<String> {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        cfg.inference_endpoints()
            .endpoints
            .iter()
            .filter(|e| {
                !crate::engines::http_provider::HttpProvider::resolve_api_key(
                    &e.api_key_env,
                    &e.name,
                )
                .is_empty()
                    || e.api_base.contains("localhost")
                    || e.api_base.contains("127.0.0.1")
            })
            .map(|e| e.name.clone())
            .collect()
    }

    fn provider_cooled(&self, endpoint: &str) -> bool {
        crate::engines::routing::InferenceRouter::provider_cooled(endpoint)
    }
}

/// One engine call's parameters: the prompt, the streaming callback and
/// the caller's model preference.
pub struct ContractCall<'c> {
    pub prompt: String,
    pub callback: &'c dyn Fn(String),
    pub model_hint: Option<String>,
    /// The same request as a conversation with tool schemas
    /// (`{"messages": [..], "tools": [..]}`), for an engine that renders the
    /// model's own chat template. `prompt` stays the complete fallback.
    pub chat: Option<serde_json::Value>,
}

impl<'c> ContractCall<'c> {
    pub fn new(prompt: &str, callback: &'c dyn Fn(String), model_hint: Option<&str>) -> Self {
        Self {
            prompt: prompt.to_string(),
            callback,
            model_hint: model_hint.map(str::to_string),
            chat: None,
        }
    }

    #[must_use]
    pub fn with_chat(mut self, chat: Option<serde_json::Value>) -> Self {
        self.chat = chat;
        self
    }
}

/// `RuntimeBackend` over one engine instance plus its host surface.
/// `loaded` mirrors the contract's own `loaded_model` — the contract
/// drops it from `Unhealthy` status, but `health` must keep probing the
/// model the runtime last staged.
pub struct NativeEngineBackend<'c> {
    engine: Arc<dyn NativeInferenceEngine>,
    host: Arc<dyn EngineHost>,
    kind: EngineKind,
    call: &'c ContractCall<'c>,
    loaded: Mutex<Option<String>>,
}

impl<'c> NativeEngineBackend<'c> {
    pub fn new(
        engine: Arc<dyn NativeInferenceEngine>,
        host: Arc<dyn EngineHost>,
        kind: EngineKind,
        call: &'c ContractCall<'c>,
    ) -> Self {
        Self {
            engine,
            host,
            kind,
            call,
            loaded: Mutex::new(None),
        }
    }

    fn resolved_model(&self) -> Option<String> {
        if let Some(m) = self
            .loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Some(m);
        }
        match self.kind {
            EngineKind::Native => self
                .host
                .resolve_model(self.call.model_hint.as_deref(), &self.call.prompt),
            // A federated "model" is its delegation target.
            EngineKind::Federated => self
                .call
                .model_hint
                .clone()
                .or_else(|| self.host.endpoints().first().cloned()),
        }
    }
}

impl RuntimeBackend for NativeEngineBackend<'_> {
    fn discover(&self) -> EaiResult<bool> {
        match self.kind {
            EngineKind::Native => Ok(self.resolved_model().is_some()),
            EngineKind::Federated => Ok(!self.host.endpoints().is_empty()),
        }
    }

    fn load(&self, model: &str) -> EaiResult<()> {
        match self.kind {
            EngineKind::Native => self.host.preload(model)?,
            EngineKind::Federated => {
                // No weights to stage: the delegation target must exist.
                if self.host.endpoints().is_empty() {
                    return Err(EaiError::inference(
                        "no resolvable federated endpoint to delegate to",
                    ));
                }
            }
        }
        *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = Some(model.to_string());
        Ok(())
    }

    fn health(&self) -> EaiResult<Result<(), String>> {
        match self.kind {
            EngineKind::Native => {
                let Some(model) = self.resolved_model() else {
                    return Ok(Err("no model resolved for this engine".to_string()));
                };
                match self.host.model_resident(&model)? {
                    true => Ok(Ok(())),
                    false => Ok(Err(format!(
                        "weights for '{model}' are not resident in the substrate"
                    ))),
                }
            }
            EngineKind::Federated => {
                let live = self
                    .host
                    .endpoints()
                    .into_iter()
                    .filter(|e| !self.host.provider_cooled(e))
                    .count();
                if live > 0 {
                    Ok(Ok(()))
                } else {
                    Ok(Err(
                        "every resolvable federated endpoint is cooled".to_string()
                    ))
                }
            }
        }
    }

    fn infer(&self, prompt: &str) -> EaiResult<InferOutcome> {
        let model = self.resolved_model();

        let answer = match &self.call.chat {
            Some(chat) => {
                self.engine
                    .run_chat_stream(prompt, chat, self.call.callback, model.as_deref())
            }
            None => self
                .engine
                .run_inference_stream(prompt, self.call.callback, model.as_deref()),
        };
        match answer {
            Ok(output) if !output.trim().is_empty() => Ok(InferOutcome::Done { output }),
            Ok(_) => Ok(InferOutcome::Unavailable {
                reason: "engine returned empty output".to_string(),
            }),
            Err(e) => Ok(InferOutcome::Unavailable {
                reason: e.to_string(),
            }),
        }
    }

    fn cancel(&self) -> EaiResult<()> {
        // Reached only when caps.can_cancel — inference tasks register
        // unscoped under SwarmTaskManager so there is no external cancel.
        Err(EaiError::inference(
            "inference task is not externally cancellable",
        ))
    }

    fn unload(&self) -> EaiResult<()> {
        let model = self.loaded.lock().unwrap_or_else(|e| e.into_inner()).take();
        match self.kind {
            EngineKind::Native => {
                let model = model.or_else(|| {
                    self.host
                        .resolve_model(self.call.model_hint.as_deref(), &self.call.prompt)
                });
                match model {
                    Some(m) => self.host.unload(&m),
                    None => Ok(()),
                }
            }
            EngineKind::Federated => Err(EaiError::inference(
                "a federated delegation has no weights to unload",
            )),
        }
    }
}

/// Drive one engine call through the lifecycle contract: discover → load
/// → ready (health probe) → infer. `kind` selects the verb mapping; a
/// native unload is a real eviction and a federated unload is a typed
/// refusal, so unsupported verbs are never silent.
pub fn infer_via_contract(
    engine: Arc<dyn NativeInferenceEngine>,
    kind: EngineKind,
    call: &ContractCall<'_>,
) -> EaiResult<String> {
    infer_via_host(engine, Arc::new(SusiHost), kind, call)
}

/// `infer_via_contract` over an explicit host — the seam tests drive.
pub fn infer_via_host(
    engine: Arc<dyn NativeInferenceEngine>,
    host: Arc<dyn EngineHost>,
    kind: EngineKind,
    call: &ContractCall<'_>,
) -> EaiResult<String> {
    let backend = NativeEngineBackend::new(engine.clone(), host, kind, call);
    let caps = Capabilities {
        can_cancel: false,
        can_unload: kind == EngineKind::Native,
    };
    let mut runtime = Runtime::new(&engine.name(), caps, &backend);
    runtime.discover()?;
    // Load the model inference will actually use — the hint when given,
    // else the engine's own resolution, so `ready` probes the real target.
    let model = call.model_hint.clone().or_else(|| backend.resolved_model());
    if let Some(model) = model {
        runtime.load(&model)?;
    }
    if !runtime.ready()? {
        return Err(EaiError::inference(format!(
            "engine {} failed its readiness probe (status {:?})",
            runtime.id,
            runtime.status()
        )));
    }
    match runtime.infer(&call.prompt)? {
        InferOutcome::Done { output } => Ok(output),
        InferOutcome::Cancelled => Err(EaiError::inference("inference cancelled")),
        InferOutcome::Unavailable { reason } => Err(EaiError::inference(reason)),
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    struct FakeEngine {
        reply: String,
    }

    impl NativeInferenceEngine for FakeEngine {
        fn name(&self) -> String {
            "fake-engine".to_string()
        }

        fn run_inference(&self, prompt: &str) -> EaiResult<String> {
            self.run_inference_stream(prompt, &|_| {}, None)
        }

        fn run_inference_stream(
            &self,
            prompt: &str,
            callback: &dyn Fn(String),
            _model_hint: Option<&str>,
        ) -> susi_error::EaiResult<String> {
            callback(self.reply.clone());
            if self.reply.is_empty() {
                Ok(String::new())
            } else {
                Ok(format!("{}:{}", prompt, self.reply))
            }
        }
    }

    struct FakeHost {
        model: Option<String>,
        resident: bool,
        endpoints: Vec<String>,
        cooled: bool,
    }

    impl EngineHost for FakeHost {
        fn resolve_model(&self, hint: Option<&str>, _prompt: &str) -> Option<String> {
            hint.map(str::to_string).or_else(|| self.model.clone())
        }

        fn model_resident(&self, _model: &str) -> Result<bool, String> {
            Ok(self.resident)
        }

        fn preload(&self, _model: &str) -> EaiResult<()> {
            Ok(())
        }

        fn unload(&self, _model: &str) -> EaiResult<()> {
            Ok(())
        }

        fn endpoints(&self) -> Vec<String> {
            self.endpoints.clone()
        }

        fn provider_cooled(&self, _endpoint: &str) -> bool {
            self.cooled
        }
    }

    #[test]
    fn contract_call_new_stores_prompt_callback_and_optional_model_hint() {
        let cb = |_: String| {};
        let call = ContractCall::new("solve x", &cb, Some("qwen-7b"));
        assert_eq!(call.prompt, "solve x");
        assert_eq!(call.model_hint.as_deref(), Some("qwen-7b"));
    }

    #[test]
    fn native_engine_backend_discover_load_health_and_unload_drive_host_seams() {
        let cb = |_: String| {};
        let call = ContractCall::new("hello", &cb, None);
        let engine: Arc<dyn NativeInferenceEngine> = Arc::new(FakeEngine {
            reply: "ok".to_string(),
        });
        let host: Arc<dyn EngineHost> = Arc::new(FakeHost {
            model: Some("local-m1".to_string()),
            resident: true,
            endpoints: Vec::new(),
            cooled: false,
        });
        let backend = NativeEngineBackend::new(engine, host, EngineKind::Native, &call);
        assert!(backend.discover().expect("discover"));
        backend.load("local-m1").expect("load");
        assert!(backend.health().expect("health").is_ok());
        backend.unload().expect("unload");
        assert!(backend.cancel().is_err());
    }

    #[test]
    fn federated_engine_backend_validates_endpoints_cooldown_and_refuses_unload() {
        let cb = |_: String| {};
        let call = ContractCall::new("hello", &cb, None);
        let engine: Arc<dyn NativeInferenceEngine> = Arc::new(FakeEngine {
            reply: "ok".to_string(),
        });
        let cooled_host: Arc<dyn EngineHost> = Arc::new(FakeHost {
            model: None,
            resident: false,
            endpoints: vec!["ep-1".to_string()],
            cooled: true,
        });
        let backend = NativeEngineBackend::new(engine, cooled_host, EngineKind::Federated, &call);
        assert!(backend.discover().expect("discover"));
        assert!(backend.health().expect("health").is_err());
        assert!(backend.unload().is_err());
    }

    #[test]
    fn infer_via_host_runs_full_discover_load_ready_infer_contract_and_rejects_empty_output() {
        let cb = |_: String| {};
        let call = ContractCall::new("prompt-1", &cb, Some("m1"));
        let ok_engine: Arc<dyn NativeInferenceEngine> = Arc::new(FakeEngine {
            reply: "done".to_string(),
        });
        let host: Arc<dyn EngineHost> = Arc::new(FakeHost {
            model: Some("m1".to_string()),
            resident: true,
            endpoints: Vec::new(),
            cooled: false,
        });
        let out = infer_via_host(ok_engine, Arc::clone(&host), EngineKind::Native, &call)
            .expect("infer via host");
        assert_eq!(out, "prompt-1:done");

        let empty_engine: Arc<dyn NativeInferenceEngine> = Arc::new(FakeEngine {
            reply: String::new(),
        });
        let err = infer_via_host(empty_engine, host, EngineKind::Native, &call)
            .expect_err("empty output must fail");
        assert!(err.to_string().contains("empty output"));
    }

    #[test]
    fn infer_via_host_fails_when_native_model_is_not_resident() {
        let cb = |_: String| {};
        let call = ContractCall::new("prompt-2", &cb, Some("m1"));
        let ok_engine: Arc<dyn NativeInferenceEngine> = Arc::new(FakeEngine {
            reply: "done".to_string(),
        });
        let unready_host: Arc<dyn EngineHost> = Arc::new(FakeHost {
            model: Some("m1".to_string()),
            resident: false,
            endpoints: Vec::new(),
            cooled: false,
        });
        let err = infer_via_host(ok_engine, unready_host, EngineKind::Native, &call)
            .expect_err("non-resident model must fail readiness check");
        assert!(!err.to_string().is_empty());
    }
}
