//! A local-inference request that reaches a wired local-engine plug (the
//! commercial `susi-plug-host`) must name the model core selected and the
//! weights core resolved for it, so the plug loads exactly that model or
//! refuses naming it. Drives the production local rung
//! (`infer_via_contract`: discover -> load -> ready -> infer) against a
//! recording `gpu_fabric.local_engine.*` handler on the plane bus.
#![allow(missing_docs)] // integration test crate: no public API to document
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use susi_gemi::engines::local_plug::{LOCAL_ENGINE_BUS_PREFIX, TOPIC_LOCAL_INFER_STREAM};
use susi_gemi::engines::native_lifecycle::{infer_via_contract, ContractCall, EngineKind};
use susi_gemi::engines::runtime::{NativeInferenceEngine, SusiGgufEngine};
use susi_gemi::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use susi_paths::test_env::EnvGuard;

/// Stands in for the plug host: records every infer payload and answers.
struct RecordingPlug(Mutex<Vec<Value>>);

impl PlaneHandler for RecordingPlug {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        if topic != TOPIC_LOCAL_INFER_STREAM {
            return Err(format!("unexpected topic {topic}"));
        }
        self.0.lock().unwrap().push(payload);
        Ok(json!({ "text": "answered by the plug" }))
    }
}

/// Smallest GGUF the substrate accepts: magic, version 3, no tensors, no
/// metadata (architecture defaults to llama).
fn tiny_gguf() -> Vec<u8> {
    let mut b = b"GGUF".to_vec();
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b
}

#[test]
fn plug_call_names_the_selected_model_and_its_resolved_weights() {
    // This file holds a single test, so its process environment is its own.
    let root = susi_paths::tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let models = root.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    // The chosen model, plus a decoy whose name contains the chosen id.
    let chosen = models.join("tiny-chosen.gguf");
    std::fs::write(&chosen, tiny_gguf()).unwrap();
    std::fs::write(models.join("tiny-chosen-decoy.gguf"), tiny_gguf()).unwrap();
    let mut env = EnvGuard::isolated();
    env.set("HOME", &home)
        .set("SUSI_HOME", home.join(".susi"))
        .set("XDG_CACHE_HOME", home.join(".cache"))
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set("XDG_DATA_HOME", home.join(".local/share"))
        .set("SUSI_MODEL_DIR", &models)
        .remove("SUSI_TEST_MOCK_INFERENCE");

    let plug = Arc::new(RecordingPlug(Mutex::new(Vec::new())));
    PlaneBus::global().register_prefix(LOCAL_ENGINE_BUS_PREFIX, plug.clone());

    let out = Mutex::new(String::new());
    let callback = |chunk: String| out.lock().unwrap().push_str(&chunk);
    let engine: Arc<dyn NativeInferenceEngine> = Arc::new(SusiGgufEngine);
    let answer = infer_via_contract(
        engine,
        EngineKind::Native,
        &ContractCall::new("say hi", &callback, Some("tiny-chosen")),
    )
    .expect("the wired plug answers");

    assert_eq!(answer, "answered by the plug");
    assert_eq!(*out.lock().unwrap(), "answered by the plug");
    let calls = plug.0.lock().unwrap();
    assert_eq!(calls.len(), 1, "one infer call reaches the plug: {calls:?}");
    let call = &calls[0];
    assert_eq!(call["prompt"], "say hi");
    assert_eq!(call["selected_model"], "tiny-chosen", "{call}");
    assert_eq!(
        call["model_path"].as_str().map(std::path::PathBuf::from),
        Some(chosen),
        "the plug is told the exact weights core resolved: {call}"
    );
}
