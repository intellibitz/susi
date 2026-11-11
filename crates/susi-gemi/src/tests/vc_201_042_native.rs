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

#[test]
fn vc_201_042_native_discovered_path_reaches_weight_loader() {
    use crate::engines::native_lifecycle::SusiHost;
    use crate::engines::runtime::InferenceHost;

    let _guard = crate::engines::env_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", dir.path());
    env.set("SUSI_HOME", dir.path());
    let weights = dir.path().join("discovered.gguf");
    std::fs::write(&weights, b"invalid weights").unwrap();
    let model = weights.to_str().unwrap();
    let host = SusiHost;
    assert_eq!(
        host.resolve_model(Some(model), "hello").as_deref(),
        Some(model)
    );
    let error = host.preload(model).unwrap_err().to_string();
    assert!(
        !error.contains("model must be a model id or file name"),
        "{error}"
    );
    // Failed loading cannot pretend to be resident, but exact-path eviction
    // remains valid. The public remote-management boundary still rejects paths.
    assert!(!host.model_resident(model).unwrap());
    host.unload(model).unwrap();
    assert!(InferenceHost::preload(model).is_err());
    assert!(InferenceHost::unload(model).is_err());
}

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

// ── Conversations and tool lists (T-CLAUDELOCALTOOLS-1) ──────────────────

/// An engine that knows conversations, and records what it was given.
#[derive(Default)]
struct ChatEngine {
    chats: Mutex<Vec<(String, serde_json::Value)>>,
    plain: Mutex<Vec<String>>,
}

impl NativeInferenceEngine for ChatEngine {
    fn name(&self) -> String {
        "ChatNative".to_string()
    }
    fn run_inference(&self, prompt: &str) -> EaiResult<String> {
        self.run_inference_stream(prompt, &|_| {}, None)
    }
    fn run_inference_stream(
        &self,
        prompt: &str,
        _callback: &dyn Fn(String),
        _selected_model: Option<&str>,
    ) -> EaiResult<String> {
        self.plain.lock().unwrap().push(prompt.to_string());
        Ok("plain answer".to_string())
    }
    fn run_chat_stream(
        &self,
        prompt: &str,
        chat: &serde_json::Value,
        _callback: &dyn Fn(String),
        _selected_model: Option<&str>,
    ) -> EaiResult<String> {
        self.chats
            .lock()
            .unwrap()
            .push((prompt.to_string(), chat.clone()));
        Ok("chat answer".to_string())
    }
}

fn answer(engine: Arc<dyn NativeInferenceEngine>, call: &ContractCall<'_>) -> String {
    let host = Arc::new(FakeHost::default());
    host.models.lock().unwrap().push("m".to_string());
    infer_via_host(engine, host, EngineKind::Native, call).unwrap()
}

#[test]
fn a_request_with_tools_reaches_the_engine_as_a_conversation_and_keeps_its_prose_fallback() {
    let listed = serde_json::json!([
        {"name": "get_time", "description": "Current time in a city such as Paris", "input_schema": {
            "type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}},
        {"name": "paris_time_zone"},
        {"name": "brain_matrix", "description": "Show the measured model capability matrix"},
        {"description": "no name: not a tool the model can call"}
    ]);
    let chat =
        crate::local_tool_calls::chat_request_from(&listed, "what time is it in Paris?").unwrap();
    assert_eq!(
        chat["messages"],
        serde_json::json!([{"role": "user", "content": "what time is it in Paris?"}])
    );
    let tools = chat["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "get_time");
    assert_eq!(tools[0]["function"]["parameters"]["required"][0], "city");
    // Ranked by fit (get_time matches "time" and "paris" plus its description;
    // paris_time_zone only its name), an unrelated tool is not offered, and a
    // tool that publishes no schema still gets a valid empty one.
    assert_eq!(tools[1]["function"]["name"], "paris_time_zone");
    assert_eq!(
        tools[1]["function"]["parameters"],
        serde_json::json!({"type": "object", "properties": {}})
    );
    // Nothing registered, or nothing that fits: no conversation to send, so
    // the model answers in prose instead of calling an unrelated tool.
    let unrelated = serde_json::json!([{"name": "benchmark_run", "description": "Measure local inference latency"}]);
    assert!(crate::local_tool_calls::chat_request_from(
        &unrelated,
        "What is the weather in Paris right now?"
    )
    .is_none());
    assert!(crate::local_tool_calls::chat_request_from(&serde_json::json!([]), "hi").is_none());
    assert!(
        crate::local_tool_calls::chat_request_from(&serde_json::json!({"error": "x"}), "hi")
            .is_none()
    );

    // The contract hands both to an engine that knows conversations...
    let engine = Arc::new(ChatEngine::default());
    let call = ContractCall::new("PROSE TOOLS\n\nwhat time is it in Paris?", &noop, Some("m"))
        .with_chat(Some(chat.clone()));
    assert_eq!(answer(engine.clone(), &call), "chat answer");
    let seen = engine.chats.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].0.starts_with("PROSE TOOLS"),
        "the prose prompt is still sent"
    );
    assert_eq!(seen[0].1, chat);
    assert!(engine.plain.lock().unwrap().is_empty());
    drop(seen);

    // ...an engine that does not know them gets the prose prompt, as before...
    let old = FakeEngine::with_output("old answer");
    assert_eq!(answer(old.clone(), &call), "old answer");
    assert!(old.calls.lock().unwrap()[0].0.starts_with("PROSE TOOLS"));

    // ...and a request without tools is a plain prompt for every engine.
    let plain = ContractCall::new("hello", &noop, Some("m"));
    assert_eq!(answer(engine.clone(), &plain), "plain answer");
    assert_eq!(engine.plain.lock().unwrap().as_slice(), ["hello"]);
}

#[test]
fn the_plug_request_carries_the_conversation_only_when_there_is_one() {
    use crate::engines::local_plug::infer_payload;
    let chat = serde_json::json!({
        "messages": [{"role": "user", "content": "q"}],
        "tools": [{"type": "function", "function": {"name": "t"}}],
        "ignored": true
    });
    let with = infer_payload("prose", &chat, Some("m"), None);
    assert_eq!(with["prompt"], "prose");
    assert_eq!(with["selected_model"], "m");
    assert_eq!(with["messages"], chat["messages"]);
    assert_eq!(with["tools"], chat["tools"]);
    assert!(with.get("ignored").is_none());
    // A plain request is byte for byte what it was before.
    assert_eq!(
        infer_payload("prose", &serde_json::Value::Null, Some("m"), None),
        serde_json::json!({"prompt": "prose", "selected_model": "m", "model_path": null})
    );
}

#[test]
fn calls_to_caller_supplied_tools_are_returned_not_run() {
    use crate::susi_core::plane_bus::gemi::CALLER_TOOL_CALLS;
    let reply = crate::local_tool_calls::caller_tool_reply(
        "Let me check.\n<tool_call>\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Paris\"}}\n</tool_call>",
    );
    let body: serde_json::Value =
        serde_json::from_str(reply.strip_prefix(CALLER_TOOL_CALLS).unwrap()).unwrap();
    assert_eq!(body["content"], "Let me check.");
    assert_eq!(body["tool_calls"][0]["name"], "get_weather");
    assert_eq!(body["tool_calls"][0]["arguments"]["city"], "Paris");
    // Text without a call is the answer as it is.
    assert_eq!(
        crate::local_tool_calls::caller_tool_reply("The sky is blue."),
        "The sky is blue."
    );
}

#[test]
fn max_tokens_reaches_the_engine_request() {
    let chat = serde_json::json!({"messages": [{"role": "user", "content": "q"}], "max_tokens": 5});
    let payload = crate::engines::local_plug::infer_payload("q", &chat, None, None);
    assert_eq!(payload["max_tokens"], 5);
}
