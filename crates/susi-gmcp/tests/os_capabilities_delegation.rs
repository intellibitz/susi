//! `os_capabilities` issues and admits host-signed delegation envelopes
//! across processes (Mandate 73 recursion adapter, T-CLAUDERECURSE-1 and
//! T-CLAUDESIGNENV-1).
//!
//! The test re-runs its own binary as child processes, each with `SUSI_HOME`
//! on a private scratch directory, so the host MAC key the envelope key is
//! derived from is created there and never in a real home. One child issues
//! an envelope through the real `delegate` action; a second child sharing the
//! same home (the same host key) admits it through `invoke`; a child on
//! another home (another host key) refuses it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use susi_gmcp::susi_core::registry::{CapabilityRegistry, Tool};
use susi_gmcp::susi_error::EaiResult;
use susi_gmcp::tools::args::core_input_schema;
use susi_gmcp::tools::CoreTools;

const TEST: &str = "os_capabilities_signs_and_admits_envelopes_across_processes";
const ROLE: &str = "SUSI_DELEGATION_TEST_ROLE";
const ENVELOPE: &str = "SUSI_DELEGATION_TEST_ENVELOPE";
const MARK: &str = "DELEGATION-ENVELOPE=";
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

fn call(arg: Value) -> EaiResult<Value> {
    let text = CoreTools::os_capabilities(&arg, &std::env::temp_dir())?;
    Ok(serde_json::from_str(&text).unwrap())
}

fn invoke(permission: &str, delegation: Value) -> EaiResult<Value> {
    call(json!({
        "action": "invoke",
        "id": TOOL,
        "subject": "opus",
        "permission": permission,
        "delegation": delegation,
    }))
}

fn hops(receipt: &Value) -> Vec<String> {
    receipt["delegation_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hop| format!("{}:{}", hop["component"], hop["capability"]).replace('"', ""))
        .collect()
}

/// Child role `issue`: a driver asks the host for an envelope for `dot` to
/// run `plan`, then for a nested one under it, and prints the nested one.
fn issue() {
    let root = call(json!({
        "action": "delegate",
        "id": "plan",
        "subject": "dot",
        "permission": "capability:tool",
        "budget_micros": 100,
        "timeout_ms": 60_000,
    }))
    .unwrap();
    assert_eq!(root["path"][1]["component"], "dot");
    assert!(root["signature"].is_string() && root["id"].is_string());
    // Nesting under the issued envelope cannot widen it.
    let wider = call(json!({
        "action": "delegate",
        "id": "build",
        "subject": "carbon",
        "permission": "capability:other",
        "delegation": root,
    }))
    .unwrap_err()
    .to_string();
    assert!(wider.contains("PermissionAmplification"), "{wider}");
    let nested = call(json!({
        "action": "delegate",
        "id": "build",
        "subject": "carbon",
        "permission": "capability:tool",
        "budget_micros": 40,
        "delegation": root,
    }))
    .unwrap();
    assert_eq!(nested["budget_micros"], 40);
    println!("{MARK}{nested}");
}

/// Child role `admit`: the same host (same `SUSI_HOME`) runs the call the
/// envelope's holder asks for.
fn admit(envelope: Value) {
    CapabilityRegistry::global().register_tool(Probe);
    let receipt = invoke("capability:tool", envelope.clone()).unwrap();
    assert_eq!(receipt["output"], "probed");
    assert_eq!(
        hops(&receipt),
        [
            "dot:root",
            "dot:plan",
            "carbon:build",
            format!("opus:{TOOL}").as_str()
        ]
    );
    // The tool's own permission is checked first: presenting another one
    // is refused before the envelope is consulted.
    let err = invoke("capability:other", envelope.clone())
        .unwrap_err()
        .to_string();
    assert!(err.contains("PermissionDenied"), "{err}");
    let mut wider = envelope.clone();
    wider["budget_micros"] = json!(1_000_000);
    let err = invoke("capability:tool", wider).unwrap_err().to_string();
    assert!(err.contains("Unverified"), "{err}");
    let mut unsigned = envelope;
    unsigned.as_object_mut().unwrap().remove("signature");
    let err = invoke("capability:tool", unsigned).unwrap_err().to_string();
    assert!(err.contains("Unverified"), "{err}");
    assert!(CapabilityRegistry::global().unregister_tool(TOOL));
}

/// Child role `foreign`: another host key refuses the envelope.
fn foreign(envelope: Value) {
    let err = invoke("capability:tool", envelope).unwrap_err().to_string();
    assert!(err.contains("Unverified"), "{err}");
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-delegation-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether `dir` (recursively) holds the host MAC key file.
fn holds_key(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.file_name().is_some_and(|name| name == "mac.hmac.key")
                || (path.is_dir() && holds_key(&path))
        })
}

fn child(role: &str, home: &Path, envelope: Option<&str>) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env("SUSI_HOME", home)
        .env_remove("SUSI_HERMETIC_FORBIDDEN");
    if let Some(envelope) = envelope {
        command.env(ENVELOPE, envelope);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{role} child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn os_capabilities_signs_and_admits_envelopes_across_processes() {
    if let Ok(role) = std::env::var(ROLE) {
        let envelope = || serde_json::from_str(&std::env::var(ENVELOPE).unwrap()).unwrap();
        match role.as_str() {
            "issue" => issue(),
            "admit" => admit(envelope()),
            "foreign" => foreign(envelope()),
            other => panic!("unknown role {other}"),
        }
        return;
    }
    let schema = core_input_schema("os_capabilities").expect("os_capabilities is a core tool");
    assert!(
        schema["properties"].get("delegation").is_some(),
        "the typed schema advertises the delegation argument: {schema:?}"
    );

    let host = scratch("host");
    let other = scratch("other-host");
    let issued = child("issue", &host, None);
    let envelope = issued
        .lines()
        .find_map(|line| line.split_once(MARK).map(|(_, json)| json))
        .unwrap_or_else(|| panic!("no envelope printed:\n{issued}"))
        .to_string();
    assert!(
        holds_key(&host),
        "the host MAC key was created in the scratch home"
    );
    child("admit", &host, Some(&envelope));
    child("foreign", &other, Some(&envelope));
    let _ = std::fs::remove_dir_all(&host);
    let _ = std::fs::remove_dir_all(&other);
}
