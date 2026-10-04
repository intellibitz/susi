//! Native inference engines driven through the shared lifecycle contract
//! (VC-201-042, T-DEEPSEEK-231).
//!
//! `LlamaCppEngine`/`SusiGgufEngine` and `SusiFederatedEngine` are adapted
//! onto `runtime_lifecycle::RuntimeBackend` so the production engine call
//! runs discover → load → ready → infer through the one contract.
//! `InferenceHost` is the adapter's substrate surface: `load` stages
//! weights through `InferenceHost::preload`, `health` probes
//! `InferenceHost::loaded_models` (weight residency, never file
//! presence), and `unload` evicts through `InferenceHost::unload`.
//! Verbs a native engine genuinely lacks — external cancel of an
//! unscoped inference task, unload of a federated endpoint — are
//! capability-gated to typed errors, not silent no-ops.

use std::path::PathBuf;
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
        for entry in crate::engines::runtime::InferenceHost::loaded_models() {
            let matches = entry
                .get("path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from)
                .is_some_and(|p| p == path || p.file_name() == path.file_name());
            let resident = entry
                .get("state")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s == "loaded" || s == "busy");
            if matches && resident {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn preload(&self, model: &str) -> EaiResult<()> {
        crate::engines::runtime::InferenceHost::preload(model).map(|_| ())
    }

    fn unload(&self, model: &str) -> EaiResult<()> {
        crate::engines::runtime::InferenceHost::unload(model).map(|_| ())
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
}

impl<'c> ContractCall<'c> {
    pub fn new(prompt: &str, callback: &'c dyn Fn(String), model_hint: Option<&str>) -> Self {
        Self {
            prompt: prompt.to_string(),
            callback,
            model_hint: model_hint.map(str::to_string),
        }
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
        match self
            .engine
            .run_inference_stream(prompt, self.call.callback, model.as_deref())
        {
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
