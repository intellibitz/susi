#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use susi_core::plane_bus::{PlaneBus, PlaneHandler};
use susi_gemi_models::hf_discovery::{resolve_bootstrap_ladder, resolve_model_ladder};
use susi_sandbox_client::manager::SusiConfig;

const TOPIC: &str = "ecosystem_hub.models.discovery.v1";

struct Host {
    indices: Mutex<Value>,
    requests: Mutex<Vec<Value>>,
}

impl PlaneHandler for Host {
    fn handle(&self, topic: &str, request: Value) -> Result<Value, String> {
        assert_eq!(topic, TOPIC);
        self.requests.lock().unwrap().push(request.clone());
        Ok(json!({
            "version":2, "mode":request["mode"], "selection_sha256":request["selection_sha256"],
            "result":{"status":"ready", "admitted_indices":*self.indices.lock().unwrap()}
        }))
    }
}

#[test]
fn public_resolvers_preserve_caller_metadata_and_validate_observed_indices() {
    let home = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", home.path()).remove("HF_TOKEN");
    // The source port has no HTTP server. A local fallback cannot admit a model.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", port.local_addr().unwrap());
    drop(port);
    let steps = json!([
        {"hf_repo":"org/shared","hf_file":"model-Q4_0.gguf","step":11,"label":"local first","operator_note":"confidential"},
        {"hf_repo":"org/denied","hf_file":"model-Q4_0.gguf","step":17},
        {"hf_repo":"org/shared","hf_file":"model-Q4_0.gguf","step":23,"label":"local duplicate","other":{"x":73}},
        {"hf_repo":"org/full","hf_file":"model-F16.gguf"}
    ]);
    let cfg: SusiConfig =
        serde_json::from_value(json!({"hf_base_url":base,"model_ladder":steps})).unwrap();
    let host = Arc::new(Host {
        indices: Mutex::new(json!([0, 2])),
        requests: Mutex::new(Vec::new()),
    });
    let bus = PlaneBus::global();
    bus.register(TOPIC, host.clone());
    let input = cfg.model_ladder();
    let expected = json!([input[0], input[2]]);
    for resolved in [resolve_model_ladder(&cfg), resolve_bootstrap_ladder(&cfg)] {
        assert_eq!(serde_json::to_value(resolved).unwrap(), expected);
    }
    let artifacts = [
        ("org/shared", "model-Q4_0.gguf", ""),
        ("org/denied", "model-Q4_0.gguf", ""),
        ("org/shared", "model-Q4_0.gguf", ""),
        ("org/full", "model-F16.gguf", ""),
    ];
    let digest = susi_gemi_models::resumable_downloads::sha256_hex(
        &serde_json::to_vec(&("susi-model-admission-v2", &base, artifacts)).unwrap(),
    );
    for (request, mode) in host
        .requests
        .lock()
        .unwrap()
        .iter()
        .zip(["catalog", "bootstrap"])
    {
        assert_eq!(
            request,
            &json!({"version":2,"mode":mode,"selection_sha256":digest,"cache_only":true})
        );
    }
    for malformed in [
        json!([2, 0]),
        json!([0, 0]),
        json!([0, 8]),
        json!([1.5]),
        json!([3]),
    ] {
        *host.indices.lock().unwrap() = malformed;
        assert!(resolve_model_ladder(&cfg).is_empty());
    }
    *host.indices.lock().unwrap() = json!([0, 2]);
    let seen = host.requests.lock().unwrap().len();
    env.set("HF_TOKEN", "private-fixture");
    assert!(resolve_model_ladder(&cfg).is_empty());
    assert_eq!(
        host.requests.lock().unwrap().len(),
        seen,
        "a private caller reached the host"
    );
    env.remove("HF_TOKEN");
    assert!(bus.unregister(TOPIC));
    assert!(resolve_bootstrap_ladder(&cfg).is_empty());
}
