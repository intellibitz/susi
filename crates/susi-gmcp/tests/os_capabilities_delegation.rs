//! `os_capabilities` continues a driver's delegation scope over the socket
//! (Mandate 73 recursion adapter, T-CLAUDERECURSE-1): a call carrying a
//! `delegation` envelope nests under it, cannot exceed it, and its receipt
//! carries the full path; malformed or cyclic envelopes are refused.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use serde_json::{json, Value};
use std::path::Path;
use susi_gmcp::susi_core::delegation::{
    DelegationHop, DelegationLimits, DelegationRequest, DelegationScope,
};
use susi_gmcp::susi_core::registry::{CapabilityRegistry, Tool};
use susi_gmcp::susi_error::EaiResult;
use susi_gmcp::tools::args::core_input_schema;
use susi_gmcp::tools::CoreTools;

const TOOL: &str = "delegation_probe_tool";

struct Probe;
impl Tool for Probe {
    fn name(&self) -> &str {
        TOOL
    }
    fn description(&self) -> &str {
        "returns a fixed string"
    }
    fn execute(&self, _args: &Value, _workspace: &Path) -> EaiResult<String> {
        Ok("probed".into())
    }
}

fn driver_envelope(permission: &str) -> Value {
    let session = DelegationScope::root(
        DelegationHop::new("ceo-session", "root").unwrap(),
        [permission.to_string()],
        1_000,
        Some(60_000),
        DelegationLimits::default(),
    )
    .unwrap();
    let driver = session
        .delegate(DelegationRequest {
            hop: DelegationHop::new("dot", "plan").unwrap(),
            permissions: [permission.to_string()].into(),
            budget_micros: 100,
            timeout_ms: 30_000,
        })
        .unwrap();
    serde_json::to_value(driver.envelope()).unwrap()
}

fn invoke(extra: Value) -> EaiResult<Value> {
    let mut arg = json!({
        "action": "invoke",
        "id": TOOL,
        "subject": "opus",
        "permission": "capability:tool",
    });
    for (key, value) in extra.as_object().unwrap() {
        arg[key] = value.clone();
    }
    let text = CoreTools::os_capabilities(&arg, &std::env::temp_dir())?;
    Ok(serde_json::from_str(&text).unwrap())
}

fn hops(receipt: &Value) -> Vec<String> {
    receipt["delegation_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hop| format!("{}:{}", hop["component"], hop["capability"]).replace('"', ""))
        .collect()
}

#[test]
fn os_capabilities_nests_under_a_driver_envelope() {
    let schema = core_input_schema("os_capabilities").expect("os_capabilities is a core tool");
    assert!(
        schema["properties"].get("delegation").is_some(),
        "the typed schema advertises the delegation argument: {schema:?}"
    );

    CapabilityRegistry::global().register_tool(Probe);

    let top = invoke(json!({})).unwrap();
    assert_eq!(top["output"], "probed");
    assert_eq!(hops(&top), ["opus:root", format!("opus:{TOOL}").as_str()]);

    let nested = invoke(json!({"delegation": driver_envelope("capability:tool")})).unwrap();
    assert_eq!(nested["output"], "probed");
    assert_eq!(
        hops(&nested),
        [
            "ceo-session:root",
            "dot:plan",
            format!("opus:{TOOL}").as_str()
        ]
    );

    // The driver never held the permission the call presents.
    let err = invoke(json!({"delegation": driver_envelope("capability:other")}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("PermissionAmplification"), "{err}");

    // Malformed and cyclic envelopes are refused before any dispatch.
    let err = invoke(json!({"delegation": {"schema": "nope"}}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("InvalidEnvelope"), "{err}");
    let mut looped = driver_envelope("capability:tool");
    looped["path"]
        .as_array_mut()
        .unwrap()
        .push(json!({"component": "dot", "capability": "plan"}));
    let err = invoke(json!({"delegation": looped}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Cycle"), "{err}");

    // Null means no envelope, exactly like omitting it.
    let null = invoke(json!({"delegation": null})).unwrap();
    assert_eq!(hops(&null).len(), 2);

    assert!(CapabilityRegistry::global().unregister_tool(TOOL));
}
