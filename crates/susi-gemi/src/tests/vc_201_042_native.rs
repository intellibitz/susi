//! VC-201-042 / T-DEEPSEEK-231: native inference engines driven through
//! the shared lifecycle contract — the same discover/load/ready/infer/
//! cancel/unload/health verbs HTTP-managed runtimes already answer.
//!
//! Hermetic by design: `EngineHost` is the substrate/routing seam, so no
//! test loads real weights or touches real endpoints.

use std::sync::{Arc, Mutex};

use crate::engines::native_lifecycle::{
    infer_via_host, ContractCall, EngineHost, EngineKind, NativeEngineBackend,
};
use crate::engines::runtime::NativeInferenceEngine;
use crate::models::runtime_lifecycle::{Capabilities, InferOutcome, Runtime};
use crate::susi_error::EaiResult;

fn noop(_: String) {}

#[derive(Default)]
struct FakeEngine {
    calls: Mutex<Vec<(String, Option<String>)>>,
    output: Mutex<String>,
}

impl FakeEngine {
    fn with_output(text: &str) -> Arc<Self> {
        let e = Self::default();
        *e.output.lock().unwrap() = text.to_string();
        Arc::new(e)
    }
}

impl NativeInferenceEngine for FakeEngine {
    fn name(&self) -> String {
        "FakeNative".to_string()
    }
    fn run_inference(&self, prompt: &str) -> EaiResult<String> {
        self.run_inference_stream(prompt, &|_| {}, None)
    }
    fn run_inference_stream(
        &self,
        prompt: &str,
        _callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String> {
        self.calls
            .lock()
            .unwrap()
            .push((prompt.to_string(), selected_model.map(str::to_string)));
        Ok(self.output.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct FakeHost {
    models: Mutex<Vec<String>>,
    resident: Mutex<Vec<String>>,
    endpoints: Mutex<Vec<String>>,
    cooled: Mutex<Vec<String>>,
    preloaded: Mutex<Vec<String>>,
    unloaded: Mutex<Vec<String>>,
}

impl EngineHost for FakeHost {
    fn resolve_model(&self, hint: Option<&str>, prompt: &str) -> Option<String> {
        let models = self.models.lock().unwrap();
        if let Some(h) = hint {
            if models.iter().any(|m| m == h) {
                return Some(h.to_string());
            }
        }
        let _ = prompt;
        models.first().cloned()
    }
    fn model_resident(&self, model: &str) -> Result<bool, String> {
        Ok(self.resident.lock().unwrap().iter().any(|m| m == model))
    }
    fn preload(&self, model: &str) -> EaiResult<()> {
        self.preloaded.lock().unwrap().push(model.to_string());
        self.resident.lock().unwrap().push(model.to_string());
        Ok(())
    }
    fn unload(&self, model: &str) -> EaiResult<()> {
        self.unloaded.lock().unwrap().push(model.to_string());
        self.resident.lock().unwrap().retain(|m| m != model);
        Ok(())
    }
    fn endpoints(&self) -> Vec<String> {
        self.endpoints.lock().unwrap().clone()
    }
    fn provider_cooled(&self, endpoint: &str) -> bool {
        self.cooled.lock().unwrap().iter().any(|e| e == endpoint)
    }
}

const NATIVE_CAPS: Capabilities = Capabilities {
    can_cancel: false,
    can_unload: true,
};

#[test]
fn vc_201_042_native_engines_through_contract_all_verbs() {
    let engine = FakeEngine::with_output("contract answer");
    let host = Arc::new(FakeHost::default());
    host.models.lock().unwrap().push("qwen-7b".to_string());
    let call = ContractCall::new("hi", &noop, Some("qwen-7b"));
    let backend = NativeEngineBackend::new(engine.clone(), host.clone(), EngineKind::Native, &call);
    let mut rt = Runtime::new("native", NATIVE_CAPS, &backend);

    rt.discover().unwrap();
    rt.load("qwen-7b").unwrap();
    assert!(rt.ready().unwrap()); // health probe passed — weights resident
    match rt.infer("hi").unwrap() {
        InferOutcome::Done { output } => assert_eq!(output, "contract answer"),
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(host.preloaded.lock().unwrap()[0], "qwen-7b");

    // cancel is honestly unsupported: typed error, never silent success.
    assert!(rt.cancel().is_err());

    rt.unload().unwrap();
    assert_eq!(host.unloaded.lock().unwrap()[0], "qwen-7b");
}

#[test]
fn vc_201_042_native_engines_through_contract_readiness_is_probe_not_presence() {
    let engine = FakeEngine::with_output("out");
    let host = Arc::new(FakeHost::default());
    host.models.lock().unwrap().push("m".to_string());
    // The weights file exists (model resolves) but is NOT resident —
    // a probe result, not presence.
    let call = ContractCall::new("p", &noop, Some("m"));
    let backend = NativeEngineBackend::new(engine.clone(), host.clone(), EngineKind::Native, &call);
    let mut rt = Runtime::new("native", NATIVE_CAPS, &backend);
    rt.discover().unwrap();
    // load stages weights → resident after preload.
    rt.load("m").unwrap();
    assert!(rt.ready().unwrap());
    // Simulate eviction mid-flight: the probe now fails, so infer refuses.
    host.resident.lock().unwrap().clear();
    assert!(!rt.ready().unwrap());
    assert!(rt.infer("p").is_err());
    assert!(engine.calls.lock().unwrap().is_empty());
}

#[test]
fn vc_201_042_native_engines_through_contract_unhealthy_reprobes() {
    let engine = FakeEngine::with_output("out");
    let host = Arc::new(FakeHost::default());
    host.models.lock().unwrap().push("m".to_string());
    let call = ContractCall::new("p", &noop, Some("m"));
    let backend = NativeEngineBackend::new(engine, host.clone(), EngineKind::Native, &call);
    let mut rt = Runtime::new("native", NATIVE_CAPS, &backend);
    rt.discover().unwrap();
    rt.load("m").unwrap();
    host.resident.lock().unwrap().clear();
    assert!(!rt.ready().unwrap());
    // An unhealthy runtime is re-probeable back to ready.
    host.resident.lock().unwrap().push("m".to_string());
    assert!(rt.ready().unwrap());
}

#[test]
fn vc_201_042_native_engines_through_contract_gone_runtime_refuses() {
    let engine = FakeEngine::with_output("out");
    let host = Arc::new(FakeHost::default()); // no models at all
    let call = ContractCall::new("p", &noop, None);
    let backend = NativeEngineBackend::new(engine, host, EngineKind::Native, &call);
    let mut rt = Runtime::new("native", NATIVE_CAPS, &backend);
    rt.discover().unwrap();
    assert!(rt.load("m").is_err()); // Gone runtime refuses the verb
}

#[test]
fn vc_201_042_native_engines_through_contract_federated_verbs() {
    let engine = FakeEngine::with_output("delegated");
    let host = Arc::new(FakeHost::default());
    host.endpoints.lock().unwrap().push("prov-a".to_string());
    let call = ContractCall::new("p", &noop, None);
    let backend = NativeEngineBackend::new(engine, host.clone(), EngineKind::Federated, &call);
    let mut rt = Runtime::new(
        "federated",
        Capabilities {
            can_cancel: false,
            can_unload: false,
        },
        &backend,
    );
    rt.discover().unwrap();
    rt.load("prov-a").unwrap(); // delegation target recorded
    assert!(rt.ready().unwrap());
    match rt.infer("p").unwrap() {
        InferOutcome::Done { .. } => {}
        other => panic!("expected Done, got {other:?}"),
    }
    // unload has no meaning for a delegation: typed error.
    assert!(rt.unload().is_err());
    // Every endpoint cooled → health probe fails → Unhealthy.
    host.cooled.lock().unwrap().push("prov-a".to_string());
    assert!(rt.health().unwrap().is_err());
}

#[test]
fn vc_201_042_native_engines_through_contract_infer_via_host_end_to_end() {
    let engine = FakeEngine::with_output("streamed answer");
    let host = Arc::new(FakeHost::default());
    host.models.lock().unwrap().push("m".to_string());
    let streamed = Mutex::new(Vec::new());
    let cb = |chunk: String| streamed.lock().unwrap().push(chunk);
    let call = ContractCall::new("question", &cb, Some("m"));
    let result = infer_via_host(engine.clone(), host.clone(), EngineKind::Native, &call).unwrap();
    assert_eq!(result, "streamed answer");
    // The resolved model inference used is the contract-loaded one.
    let calls = engine.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.as_deref(), Some("m"));
    assert_eq!(host.preloaded.lock().unwrap()[0], "m");
}

#[test]
fn vc_201_042_native_engines_through_contract_unhealthy_probe_fails_call() {
    let engine = FakeEngine::with_output("out");
    // preload reports success but weights never materialize — health
    // probe, not load's claim, decides readiness.
    struct BrokenHost(FakeHost);
    impl EngineHost for BrokenHost {
        fn resolve_model(&self, hint: Option<&str>, prompt: &str) -> Option<String> {
            self.0.resolve_model(hint, prompt)
        }
        fn model_resident(&self, model: &str) -> Result<bool, String> {
            let _ = model;
            Ok(false) // weights never resident
        }
        fn preload(&self, model: &str) -> EaiResult<()> {
            self.0.preload(model)
        }
        fn unload(&self, model: &str) -> EaiResult<()> {
            self.0.unload(model)
        }
        fn endpoints(&self) -> Vec<String> {
            self.0.endpoints()
        }
        fn provider_cooled(&self, endpoint: &str) -> bool {
            self.0.provider_cooled(endpoint)
        }
    }
    let broken = Arc::new(BrokenHost(FakeHost::default()));
    broken.0.models.lock().unwrap().push("m".to_string());
    let call = ContractCall::new("q", &noop, Some("m"));
    let err = infer_via_host(engine.clone(), broken, EngineKind::Native, &call).unwrap_err();
    assert!(err.to_string().contains("readiness probe"));
    assert!(engine.calls.lock().unwrap().is_empty()); // never dispatched
}

#[test]
fn vc_201_042_native_engines_through_contract_production_path_wired() {
    // The production local-inference call must run through the contract,
    // not the bare engine method.
    let runtime_src = include_str!("../engines/runtime.rs");
    assert!(
        runtime_src.contains("crate::engines::native_lifecycle::infer_via_contract"),
        "the local inference path must dispatch via the lifecycle contract"
    );
    let module_src = include_str!("../engines/native_lifecycle.rs");
    for verb in [
        "fn discover",
        "fn load",
        "fn health",
        "fn infer",
        "fn cancel",
        "fn unload",
    ] {
        assert!(
            module_src.contains(verb),
            "NativeEngineBackend must implement {verb}"
        );
    }
    // InferenceHost is the adapter's substrate surface.
    assert!(module_src.contains("InferenceHost::preload"));
    assert!(module_src.contains("InferenceHost::loaded_models"));
    assert!(module_src.contains("InferenceHost::unload"));
}
