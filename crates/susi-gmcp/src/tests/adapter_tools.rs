use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use std::sync::Arc;

struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        if payload["id"] == "refused" {
            return Err("refused by plane".to_string());
        }
        Ok(json!({ "payload": payload }))
    }
}

fn sent(result: EaiResult<String>) -> Value {
    serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
}

#[test]
fn adapter_tools_forward_kind_id_and_the_server_workspace() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let ws_text = ws.display().to_string();
    let doctor = sent(CoreTools::adapter_doctor(&json!({"kind": "mcp"}), &ws));
    assert_eq!(
        doctor,
        json!({"action": "doctor", "kind": "mcp", "workspace": ws_text})
    );
    let configure = sent(CoreTools::adapter_configure(
        &json!({"kind": "agents", "id": "a", "adapter": {"type": "process"}}),
        &ws,
    ));
    assert_eq!(configure["adapter"], json!({"type": "process"}));
    assert_eq!(
        sent(CoreTools::adapter_reset(
            &json!({"kind": "models", "id": "m"}),
            &ws
        ))["action"],
        "reset"
    );
    let retried = sent(CoreTools::frameworks_retry(&json!({"task_id": "t1"}), &ws));
    assert_eq!(retried["kind"], "frameworks");
    assert_eq!(retried["task_id"], "t1");
    assert_eq!(retried["workspace"], ws_text);
    let added = sent(CoreTools::mcp_add(
        &json!({"name": "n", "command_or_url": "https://mcp.example/x", "args": ["-v"]}),
        &ws,
    ));
    assert_eq!(added["args"], json!(["-v"]));
    assert!(CoreTools::mcp_add(&json!({"name": "n", "args": "-v"}), &ws).is_err());
    assert!(
        CoreTools::agents_retry(&json!({"task_id": "t", "workspace": "rel"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    assert!(CoreTools::adapter_configure(
        &json!({"kind": "agents", "id": "a", "file": "rel.json"}),
        &ws
    )
    .unwrap_err()
    .to_string()
    .contains("absolute"));
    let refused = CoreTools::adapter_setup(&json!({"kind": "mcp", "id": "refused"}), &ws);
    assert!(refused
        .unwrap_err()
        .to_string()
        .contains("refused by plane"));
}

#[test]
fn adapter_setup_and_agents_retry_forward_expected_actions_and_workspace() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let setup = sent(CoreTools::adapter_setup(
        &json!({"kind": "agents", "id": "claude"}),
        &ws,
    ));
    assert_eq!(setup["action"], "setup");
    assert_eq!(setup["kind"], "agents");
    assert_eq!(setup["id"], "claude");

    let retried = sent(CoreTools::agents_retry(&json!({"task_id": "t-42"}), &ws));
    assert_eq!(retried["action"], "retry");
    assert_eq!(retried["kind"], "agents");
    assert_eq!(retried["task_id"], "t-42");
}

#[test]
fn adapter_configure_resolves_granted_absolute_file_path() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let file = ws.join("susi-adapter-override.json");
    let configured = sent(CoreTools::adapter_configure(
        &json!({
            "kind": "frameworks",
            "id": "autogen",
            "file": file.display().to_string()
        }),
        &ws,
    ));
    assert_eq!(configured["action"], "configure");
    assert!(Path::new(configured["file"].as_str().unwrap()).ends_with("susi-adapter-override.json"));
}

#[test]
fn mcp_add_forwards_url_without_optional_args_and_rejects_bad_types() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let added = sent(CoreTools::mcp_add(
        &json!({"name": "local", "command_or_url": "https://mcp.local/sse"}),
        &ws,
    ));
    assert_eq!(added["action"], "mcp_add");
    assert_eq!(added["name"], "local");
    assert!(added.get("args").is_none());
    assert!(CoreTools::adapter_doctor(&json!({"kind": 123}), &ws).is_err());
}

#[test]
fn adapter_reset_and_frameworks_retry_reject_mistyped_or_relative_arguments() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
    let ws = std::env::temp_dir();
    assert!(CoreTools::adapter_reset(&json!({"kind": "mcp", "id": 99}), &ws).is_err());
    assert!(CoreTools::frameworks_retry(
        &json!({"task_id": "t1", "workspace": "relative/ws"}),
        &ws
    )
    .is_err());
}
