#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use susi_gmcp::GmcpHost;

struct RecordingHost(Mutex<Vec<Value>>);

impl PlaneHandler for RecordingHost {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        assert_eq!(topic, topics::TOOLS_EXECUTE);
        self.0.lock().unwrap().push(payload.clone());
        match payload["name"].as_str().unwrap() {
            "reject" => Ok(json!({"error": "Authorization Error: denied by host"})),
            "disconnect" => Err("host disconnected".into()),
            _ => Ok(json!({"text": "host result\nwith preserved whitespace"})),
        }
    }
}

struct Registration;
impl Drop for Registration {
    fn drop(&mut self) {
        PlaneBus::global().unregister(topics::TOOLS_EXECUTE);
    }
}

#[test]
fn native_dispatch_preserves_arguments_workspace_results_and_host_failures() {
    // This integration-test process owns its bus and rendezvous directory.
    let home = std::env::temp_dir().join(format!("susi-gmcp-contract-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("SUSI_HOME", &home);
    let host = Arc::new(RecordingHost(Mutex::new(Vec::new())));
    PlaneBus::global().register(topics::TOOLS_EXECUTE, host.clone());
    let _registration = Registration;
    let workspace = home.join("workspace with spaces");

    for (input, expected) in [
        (
            r#"{"path":"a b","count":2}"#,
            json!({"path":"a b","count":2}),
        ),
        (
            "plain argument\nwith whitespace",
            json!("plain argument\nwith whitespace"),
        ),
        ("[1,true,null]", json!([1, true, null])),
    ] {
        assert_eq!(
            GmcpHost::dispatch("echo", input, &workspace),
            "host result\nwith preserved whitespace"
        );
        assert_eq!(
            host.0.lock().unwrap().last().unwrap(),
            &json!({
                "name": "echo", "args": expected, "workspace": workspace.display().to_string()
            })
        );
    }
    assert_eq!(
        GmcpHost::dispatch("reject", "{}", &workspace),
        "[Error] Authorization Error: denied by host"
    );
    assert_eq!(
        GmcpHost::dispatch("disconnect", "{}", &workspace),
        "[Error] Protocol Error: host disconnected"
    );
    assert_eq!(host.0.lock().unwrap().len(), 5);
}
