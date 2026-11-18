use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use serde_json::json;
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-host-ops-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Echoes topic and payload, so a test sees exactly what a tool sent
/// over the plane bus; `{"refuse": true}` payloads come back as errors.
struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, topic: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
        if payload["name"] == "refused" {
            return Err("refused by plane".to_string());
        }
        match topic {
            topics::GAWD_AUDIT_COMPLIANCE | topics::GAWD_SUBSTRATE_REPORT => {
                Ok(json!({ "text": format!("{topic} {}", payload["workspace"]) }))
            }
            topics::GEMI_ECOSYSTEM_START => Ok(json!({ "message": "started" })),
            _ => Ok(json!({ "topic": topic, "payload": payload })),
        }
    }
}

fn wire_echo() {
    let bus = PlaneBus::global();
    for topic in [
        topics::DAEMON_PRIVACY_MODE,
        topics::DAEMON_AMBIENT_START,
        topics::DAEMON_CONTEXT_SAMPLE_OS,
        topics::DAEMON_ECOSYSTEM_SCAN,
        topics::DAEMON_SUBSTRATE_STATUS,
        topics::DAEMON_CROWN_VERIFY,
        topics::DAEMON_OS_PROVISION,
        topics::DAEMON_SERVICES_CONTROL,
        topics::GEMI_ECOSYSTEM_START,
        topics::GEMI_ECOSYSTEM_KB,
        topics::GEMI_ECOSYSTEM_EXPLAIN,
        topics::GAWD_AUDIT_COMPLIANCE,
        topics::GAWD_AUDIT_EVIDENCE_EXPORT,
        topics::GAWD_SUBSTRATE_REPORT,
    ] {
        bus.register(topic, Arc::new(Echo));
    }
}

fn doc(result: EaiResult<String>) -> serde_json::Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}

#[test]
fn broker_tools_return_the_cli_documents() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    // Read-only against the broker: a grant would persist into the
    // process bus directory, so only an unknown identity is queried.
    let ws = Path::new(".");
    let grantee = format!("host-ops-unknown-app-{}", std::process::id());
    let scope = json!({"grantee": grantee, "resource": "files", "action": "read"});
    let checked = doc(CoreTools::ipc_check(&scope, ws));
    assert_eq!(
        checked,
        json!({"grantee": grantee, "resource": "files", "action": "read", "permitted": false})
    );
    let listed = doc(CoreTools::ipc_list(&json!({"grantee": grantee}), ws));
    assert_eq!(listed, json!({"grantee": grantee, "grants": []}));
    assert_eq!(doc(CoreTools::ipc_revoke(&scope, ws))["revoked"], false);
    assert!(doc(CoreTools::ipc_pending(&json!({}), ws))["pending"].is_array());
    assert!(CoreTools::ipc_check(&json!({"grantee": grantee}), ws).is_err());
    assert!(CoreTools::ipc_list(&json!({}), ws).is_err());
    assert!(CoreTools::ipc_pending(&json!({"grantor": 3}), ws).is_err());
}

#[test]
fn intent_and_tx_lists_are_served() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let ws = temp_dir("tx");
    let intents = doc(CoreTools::intent_list(&json!({}), &ws));
    assert!(intents["providers"].is_array() && intents["needs"].is_array());
    let open = doc(CoreTools::tx_list(&json!({}), &ws));
    assert!(open["open"].is_array());
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn privacy_mode_refuses_unknown_modes_before_reaching_the_daemon() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    wire_echo();
    let ws = Path::new(".");
    let refused = CoreTools::privacy_mode(&json!({"mode": "lockdown"}), ws).unwrap_err();
    assert!(refused.to_string().contains("unknown privacy mode"));
    assert!(CoreTools::privacy_mode(&json!({}), ws).is_err());
    let sent = doc(CoreTools::privacy_mode(&json!({"mode": "local-only"}), ws));
    assert_eq!(sent["topic"], topics::DAEMON_PRIVACY_MODE);
    assert_eq!(sent["payload"]["mode"], "local-only");
    let balanced = doc(CoreTools::privacy_mode(&json!({"mode": "Balanced"}), ws));
    assert_eq!(balanced["payload"]["mode"], "Balanced");
    let host = doc(CoreTools::privacy_mode(&json!({"mode": "host-only"}), ws));
    assert_eq!(host["payload"]["mode"], "host-only");
    let typo = CoreTools::privacy_mode(&json!({"mode": "host_onyl"}), ws).unwrap_err();
    assert!(typo.to_string().contains("host_only"), "{typo}");
}

#[test]
fn plane_backed_tools_forward_the_server_workspace_and_arguments() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    wire_echo();
    let ws = temp_dir("plane");
    let ws_text = ws.display().to_string();
    let ambient = doc(CoreTools::ambient_start(&json!({}), &ws));
    assert_eq!(ambient["payload"]["workspace"], ws_text);
    let sample = doc(CoreTools::context_graph_sample_os(
        &json!({"user": "ceo"}),
        &ws,
    ));
    assert_eq!(sample["payload"]["user"], "ceo");
    assert_eq!(sample["payload"]["workspace"], ws_text);
    let tagged = doc(CoreTools::context_graph_sample_os(
        &json!({"workspace": ws_text}),
        Path::new("."),
    ));
    assert_eq!(tagged["payload"]["workspace"], ws_text);
    let relative = CoreTools::context_graph_sample_os(&json!({"workspace": "repo"}), &ws);
    assert!(relative.unwrap_err().to_string().contains("absolute"));
    let scan = doc(CoreTools::ecosystem_scan(&json!({"cached": true}), &ws));
    assert_eq!(scan["payload"]["cached"], true);
    assert!(CoreTools::ecosystem_scan(&json!({"cached": "yes"}), &ws).is_err());
    let status = doc(CoreTools::substrate_status(&json!({}), &ws));
    assert_eq!(status["topic"], topics::DAEMON_SUBSTRATE_STATUS);
    let crown = doc(CoreTools::crown_verify(&json!({}), &ws));
    assert_eq!(crown["payload"]["workspace"], ws_text);
    let audit = CoreTools::compliance_audit(&json!({}), &ws).unwrap();
    assert!(audit.starts_with(topics::GAWD_AUDIT_COMPLIANCE));
    let report = CoreTools::substrate_report(&json!({}), &ws).unwrap();
    assert!(report.starts_with(topics::GAWD_SUBSTRATE_REPORT));
    let started = doc(CoreTools::ecosystem_start(&json!({"id": "ollama"}), &ws));
    assert_eq!(started, json!({"id": "ollama", "message": "started"}));
    assert!(CoreTools::ecosystem_start(&json!({}), &ws).is_err());
    let kb = doc(CoreTools::ecosystem_kb(
        &json!({"action": "search", "query": "mcp", "ignored": 1}),
        &ws,
    ));
    assert_eq!(kb["payload"], json!({"action": "search", "query": "mcp"}));
    let explain = CoreTools::ecosystem_explain(&json!({"id": "mcp", "dir": "rel"}), &ws);
    assert!(explain.unwrap_err().to_string().contains("absolute"));
    let explained = doc(CoreTools::ecosystem_explain(&json!({"id": "mcp"}), &ws));
    assert_eq!(explained["payload"]["id"], "mcp");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn os_tools_validate_paths_and_forward_requests() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    wire_echo();
    let ws = Path::new(".");
    let probe = doc(CoreTools::os_provision(&json!({}), ws));
    assert_eq!(probe["payload"], json!({"action": "probe"}));
    let relative = CoreTools::os_provision(
        &json!({"action": "apply", "kind": "kubernetes", "manifest": "m.yaml"}),
        ws,
    );
    assert!(relative.unwrap_err().to_string().contains("absolute"));
    let manifest = std::env::temp_dir().join("m.yaml").display().to_string();
    let apply = doc(CoreTools::os_provision(
        &json!({"action": "apply", "kind": "kubernetes", "manifest": manifest}),
        ws,
    ));
    assert_eq!(apply["payload"]["manifest"], manifest);
    for (tool, action) in [
        (
            CoreTools::services_start as fn(&serde_json::Value, &Path) -> EaiResult<String>,
            "start",
        ),
        (CoreTools::services_stop, "stop"),
        (CoreTools::services_restart, "restart"),
    ] {
        let sent = doc(tool(&json!({"name": "susi-paths"}), ws));
        assert_eq!(
            sent["payload"],
            json!({"action": action, "name": "susi-paths"})
        );
        assert!(tool(&json!({}), ws).is_err());
        let refused = tool(&json!({"name": "refused"}), ws).unwrap_err();
        assert!(refused.to_string().contains("refused by plane"));
    }
    let export = CoreTools::audit_evidence_export(&json!({"input": "a", "output": "b"}), ws);
    assert!(export.unwrap_err().to_string().contains("absolute"));
    let input = std::env::temp_dir().join("in.json").display().to_string();
    let output = std::env::temp_dir().join("out.json").display().to_string();
    let exported = doc(CoreTools::audit_evidence_export(
        &json!({"input": input, "output": output}),
        ws,
    ));
    assert_eq!(exported["payload"]["output"], output);
}

#[test]
fn os_capabilities_discovers_and_validates_invocations() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let ws = Path::new(".");
    assert!(doc(CoreTools::os_capabilities(&json!({}), ws)).is_array());
    assert!(CoreTools::os_capabilities(&json!({"action": "invoke"}), ws).is_err());
    assert!(CoreTools::os_capabilities(
        &json!({"action": "invoke", "id": "x", "permission": "p", "args": [1]}),
        ws
    )
    .is_err());
    assert!(CoreTools::os_capabilities(&json!({"action": "nope"}), ws).is_err());
    // An unknown capability is refused by the bus, not dispatched.
    assert!(CoreTools::os_capabilities(
        &json!({"action": "invoke", "id": "no-such-capability", "permission": "p"}),
        ws
    )
    .is_err());
}

#[test]
fn audit_tools_read_the_server_workspace_chain() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let ws = temp_dir("audit");
    let empty = doc(CoreTools::audit_query(&json!({"actor": "nobody"}), &ws));
    assert_eq!(empty, json!([]));
    assert!(CoreTools::audit_query(&json!({"since": "yesterday"}), &ws).is_err());
    assert!(CoreTools::audit_verify(&json!({"since": -1}), &ws).is_err());
    let _ = std::fs::remove_dir_all(&ws);
}

/// Stands in for Susi Commercial Apps on `fleet.cluster.v1`: echoes the
/// action and its args, and refuses as the real roster and ledger do.
struct Cluster;
impl PlaneHandler for Cluster {
    fn handle(
        &self,
        _topic: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let action = payload["action"].as_str().unwrap_or_default().to_string();
        Ok(match action.as_str() {
            "commits_show" => {
                json!({"error": "no commit record with epoch prefix `ffff`", "kind": "config"})
            }
            "peers_probe" => json!({"error": "no signed pong", "kind": "network"}),
            "commits_compact" => json!({"error": "ledger locked", "kind": "internal"}),
            "peers_remove" => json!({"result_error": "no verified peer", "lines": [], "notes": []}),
            "peers_rekey" => {
                json!({"result_error": "rekey aborted", "lines": [], "notes": ["staged 0/1"]})
            }
            "peers_add" | "peers_unban" => {
                json!({"result_error": null, "lines": ["done"], "notes": []})
            }
            _ => json!({"action": action, "args": payload["args"]}),
        })
    }
}

#[test]
fn ledger_and_roster_tools_forward_to_fleet_cluster_and_keep_their_errors() {
    PlaneBus::global().register(topics::FLEET_CLUSTER, Arc::new(Cluster));
    let ws = Path::new(".");
    for (action, answer) in [
        ("peers_list", CoreTools::peers_list(&json!({}), ws)),
        ("peers_status", CoreTools::peers_status(&json!({}), ws)),
        ("commits_audit", CoreTools::commits_audit(&json!({}), ws)),
        ("commits_replay", CoreTools::commits_replay(&json!({}), ws)),
        ("commits_sync", CoreTools::commits_sync(&json!({}), ws)),
    ] {
        assert_eq!(doc(answer)["action"], action);
    }
    // Each refusal keeps the error class the tool raised before the move.
    let shown = CoreTools::commits_show(&json!({"epoch": "ffff"}), ws).unwrap_err();
    assert!(matches!(shown, EaiError::Config(..)), "{shown}");
    assert!(shown.to_string().contains("ffff"));
    let probed = CoreTools::peers_probe(&json!({"host": "h", "port": 9192}), ws).unwrap_err();
    assert!(matches!(probed, EaiError::Network(..)), "{probed}");
    let compacted = CoreTools::commits_compact(&json!({}), ws).unwrap_err();
    assert!(matches!(compacted, EaiError::Internal(..)), "{compacted}");
    // Arguments are still checked here before anything is sent.
    assert!(CoreTools::commits_show(&json!({}), ws).is_err());
    assert!(CoreTools::peers_probe(&json!({"host": "h", "port": 70000}), ws).is_err());
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
    // Membership answers keep their {ok, lines, notes, error} document.
    let added = doc(CoreTools::peers_add(&json!({"host": "h"}), ws));
    assert_eq!(added["ok"], true);
    assert_eq!(added["lines"], json!(["done"]));
    let rekeyed = doc(CoreTools::peers_rekey(&json!({"force": false}), ws));
    assert_eq!(rekeyed["ok"], false);
    assert_eq!(rekeyed["error"], "rekey aborted");
    assert!(CoreTools::peers_remove(&json!({"peer": "n1"}), ws).is_err());
}

#[test]
fn commit_and_peer_tools_validate_arguments() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    let ws = Path::new(".");
    assert!(CoreTools::commits_show(&json!({}), ws).is_err());
    assert!(CoreTools::peers_probe(&json!({}), ws).is_err());
    assert!(
        CoreTools::peers_probe(&json!({"host": "h", "port": 70000}), ws)
            .unwrap_err()
            .to_string()
            .contains("port")
    );
}
