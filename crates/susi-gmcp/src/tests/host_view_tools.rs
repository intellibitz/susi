use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use std::sync::Arc;

struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        Ok(json!({ "payload": payload }))
    }
}

fn sent(result: EaiResult<String>) -> Value {
    serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
}

#[test]
fn host_tools_forward_actions_and_policy_workspaces() {
    PlaneBus::global().register(topics::DAEMON_HOST, Arc::new(Echo));
    let ws = std::env::temp_dir();
    let ws_text = ws.display().to_string();
    assert_eq!(
        sent(CoreTools::operator_status(&json!({}), &ws)),
        json!({"action": "status", "workspace": ws_text})
    );
    assert_eq!(
        sent(CoreTools::operator_status(&json!({"detail": true}), &ws))["detail"],
        true
    );
    assert!(CoreTools::operator_status(&json!({"detail": "yes"}), &ws).is_err());
    assert_eq!(
        sent(CoreTools::os_status(&json!({}), &ws)),
        json!({"action": "os_status"})
    );
    assert_eq!(
        sent(CoreTools::model_uninstall(&json!({"name": "m.gguf"}), &ws))["name"],
        "m.gguf"
    );
    let ladder = sent(CoreTools::intent_ladder(
        &json!({"mode": "verify", "id": "I-1"}),
        &ws,
    ));
    assert_eq!(ladder["mode"], "verify");
    assert_eq!(ladder["workspace"], ws_text);
    assert!(
        CoreTools::os_ecosystem(&json!({"workspace": "relative"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    assert_eq!(
        sent(CoreTools::daemon_shutdown(&json!({}), &ws)),
        json!({"action": "daemon_shutdown"})
    );
}

#[test]
fn membership_tools_validate_before_touching_the_roster() {
    let ws = Path::new(".");
    assert!(CoreTools::peers_add(&json!({}), ws).is_err());
    assert!(
        CoreTools::peers_add(&json!({"host": "h", "port": 70000}), ws)
            .unwrap_err()
            .to_string()
            .contains("port")
    );
    assert!(CoreTools::peers_remove(&json!({"peer": ""}), ws).is_err());
    assert!(CoreTools::peers_rekey(&json!({"force": "yes"}), ws).is_err());
    assert!(CoreTools::extensions_create(&json!({}), ws).is_err());
}

#[test]
fn membership_returns_lines_and_the_refusal() {
    let done = membership(|out| {
        out(peer_admin::Line::Info("evicted".into()));
        out(peer_admin::Line::Note("note".into()));
        Ok(())
    })
    .unwrap();
    let doc: Value = serde_json::from_str(&done).unwrap();
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["lines"], json!(["evicted"]));
    assert_eq!(doc["notes"], json!(["note"]));
    let partial = membership(|out| {
        out(peer_admin::Line::Note("staged 0/1".into()));
        Err("rekey aborted".into())
    })
    .unwrap();
    let doc: Value = serde_json::from_str(&partial).unwrap();
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["error"], "rekey aborted");
    assert!(membership(|_| Err("no verified peer".into())).is_err());
}
