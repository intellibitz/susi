use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use serde_json::json;
use std::sync::Arc;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-control-plane-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Echoes the topic and payload, so a test sees exactly what a tool
/// sent over the plane bus.
struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, topic: &str, payload: serde_json::Value) -> Result<serde_json::Value, String> {
        if topic == topics::GAWD_PULSE_INGEST {
            return Ok(json!({ "text": format!("ingested {}", payload["intent"]) }));
        }
        Ok(json!({ "topic": topic, "payload": payload }))
    }
}

/// Run `f` with an isolated substrate root (`SUSI_HOME`) so pack state is
/// never the host's own, holding the env lock other gmcp tests share.
fn with_isolated_home(f: impl FnOnce(&Path)) {
    let _lock = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = temp_dir("home");
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", &home);
    env.remove("SUSI_EXTENSION_PACK");
    extensions::invalidate_extension_caches();
    f(&home);
    drop(env);
    extensions::invalidate_extension_caches();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn extension_tools_return_the_cli_documents_and_change_pack_state() {
    with_isolated_home(|home| {
        let ws = Path::new(".");
        let status: serde_json::Value =
            serde_json::from_str(&CoreTools::extensions_status(&json!({}), ws).unwrap()).unwrap();
        assert_eq!(status, extensions::status_report());
        assert_eq!(status["active"], "default");

        let created = extensions::create_pack("acme").unwrap();
        assert!(
            created.root.starts_with(home),
            "pack must live under the isolated home: {}",
            created.root.display()
        );
        let listed: serde_json::Value =
            serde_json::from_str(&CoreTools::extensions_list(&json!({}), ws).unwrap()).unwrap();
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == "acme" && p["seeded"] == true),
            "{listed}"
        );

        let loaded: serde_json::Value =
            serde_json::from_str(&CoreTools::extensions_load(&json!({"id": "acme"}), ws).unwrap())
                .unwrap();
        assert_eq!(loaded["loaded"]["id"], "acme");
        assert_eq!(loaded["active"], "acme");

        let unloaded: serde_json::Value = serde_json::from_str(
            &CoreTools::extensions_unload(&json!({"id": "acme"}), ws).unwrap(),
        )
        .unwrap();
        assert_eq!(unloaded, json!({"unloaded": "acme", "active": "default"}));

        assert!(CoreTools::extensions_load(&json!({"id": "missing-pack"}), ws).is_err());
        assert!(CoreTools::extensions_load(&json!({}), ws).is_err());
        assert!(CoreTools::extensions_unload(&json!({"id": " "}), ws).is_err());
    });
}

#[test]
fn extensions_validate_checks_absolute_and_workspace_relative_pack_dirs() {
    with_isolated_home(|_| {
        let ws = Path::new(".");
        let pack = extensions::create_pack("authored").unwrap();
        let report: serde_json::Value = serde_json::from_str(
            &CoreTools::extensions_validate(&json!({"path": pack.root.display().to_string()}), ws)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(report, extensions::validate_report(&pack.root).unwrap());
        assert_eq!(report["valid"], true);
        assert_eq!(report["id"], "authored");

        // A relative path resolves against the server workspace.
        let parent = pack.root.parent().unwrap();
        let relative: serde_json::Value = serde_json::from_str(
            &CoreTools::extensions_validate(&json!({"path": "authored"}), parent).unwrap(),
        )
        .unwrap();
        assert_eq!(relative, report);
        assert!(CoreTools::extensions_validate(&json!({"path": "authored"}), ws).is_err());
        assert!(CoreTools::extensions_validate(&json!({}), ws).is_err());
        let empty = temp_dir("empty-pack");
        assert!(
            CoreTools::extensions_validate(&json!({"path": empty.display().to_string()}), ws)
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&empty);
    });
}

#[test]
fn plane_backed_tools_forward_the_server_workspace() {
    let bus = PlaneBus::global();
    for topic in [
        topics::GEMI_OPERATOR_SNAPSHOT,
        topics::GAWD_PLAN_AUTONOMOUS,
        topics::GAWD_PULSE_INGEST,
    ] {
        bus.register(topic, Arc::new(Echo));
    }
    let ws = temp_dir("plane");
    let ws_text = ws.display().to_string();

    let snapshot: serde_json::Value =
        serde_json::from_str(&CoreTools::operator_snapshot(&json!({}), &ws).unwrap()).unwrap();
    assert_eq!(snapshot["topic"], topics::GEMI_OPERATOR_SNAPSHOT);
    assert_eq!(snapshot["payload"]["workspace"], ws_text);

    let plan: serde_json::Value = serde_json::from_str(
        &CoreTools::plan_run(&json!({"goal": "tidy docs", "max_steps": 5}), &ws).unwrap(),
    )
    .unwrap();
    assert_eq!(plan["payload"]["goal"], "tidy docs");
    assert_eq!(plan["payload"]["max_steps"], 5);
    assert_eq!(plan["payload"]["workspace"], ws_text);
    let default_steps: serde_json::Value =
        serde_json::from_str(&CoreTools::plan_run(&json!({"goal": "tidy docs"}), &ws).unwrap())
            .unwrap();
    assert_eq!(default_steps["payload"]["max_steps"], PLAN_MAX_STEPS);
    assert!(CoreTools::plan_run(&json!({}), &ws).is_err());
    assert!(CoreTools::plan_run(&json!({"goal": "x", "max_steps": -1}), &ws).is_err());

    let pulse = CoreTools::intent_ingest(&json!({"intent": "ship B4"}), &ws).unwrap();
    assert_eq!(pulse, "ingested \"ship B4\"");
    assert!(CoreTools::intent_ingest(&json!({}), &ws).is_err());
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn blackboard_show_reads_the_persisted_blackboard() {
    let ws = temp_dir("blackboard");
    let missing = CoreTools::blackboard_show(&json!({}), &ws).unwrap_err();
    assert!(missing.to_string().contains("no mission blackboard"));
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    let body = r#"{"kind":"mission_blackboard","entries":[],"agent_count":0}"#;
    std::fs::write(ws.join(".susi/last_blackboard.json"), body).unwrap();
    assert_eq!(CoreTools::blackboard_show(&json!({}), &ws).unwrap(), body);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn board_and_blackboard_post_tools_round_trip_with_git_board() {
    let _lock = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = temp_dir("board-gmcp");
    let remote = root.join("board-remote.git");
    std::process::Command::new("git")
        .args(["init", "--quiet", "--bare"])
        .arg(&remote)
        .status()
        .unwrap();
    let wt = root.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::process::Command::new("git")
        .current_dir(&wt)
        .args(["init", "--quiet", "-b", "wt/test"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&wt)
        .args(["config", "core.hooksPath", "no-hooks"])
        .status()
        .unwrap();
    std::process::Command::new("git")
        .current_dir(&wt)
        .args(["remote", "add", "board", remote.to_str().unwrap()])
        .status()
        .unwrap();

    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.remove("SUSI_BOARD_URL");
    env.remove("SUSI_BOARD_REMOTE_URL");
    env.remove("SUSI_BOARD_REMOTE");
    env.set("SUSI_AGENT", "gmcp-agent");

    let posted: serde_json::Value = serde_json::from_str(
        &CoreTools::board_post(
            &json!({
                "kind": "claim",
                "title": "claim from gmcp",
                "body": "testing collaboration",
                "workspace": wt.to_string_lossy()
            }),
            &wt,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(posted["n"], 1);

    let listed: serde_json::Value = serde_json::from_str(
        &CoreTools::board_list(&json!({"workspace": wt.to_string_lossy()}), &wt).unwrap(),
    )
    .unwrap();
    assert_eq!(listed["items"][0]["n"], 1);

    let checked: serde_json::Value = serde_json::from_str(
        &CoreTools::board_check(&json!({"workspace": wt.to_string_lossy()}), &wt).unwrap(),
    )
    .unwrap();
    assert_eq!(checked["items"], 1);

    let bb_posted: serde_json::Value = serde_json::from_str(
        &CoreTools::blackboard_post(
            &json!({
                "key": "status",
                "value": {"phase": "ready"},
                "workspace": wt.to_string_lossy()
            }),
            &wt,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(bb_posted["board_item"], 2);

    drop(env);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn placement_decision_reads_the_server_audit_log() {
    let ws = temp_dir("placement");
    std::fs::create_dir_all(ws.join(".susi")).unwrap();
    let entry = json!({
        "type": "INFERENCE_PLACEMENT",
        "ts": 3,
        "entry_hash": "abc",
        "details": json!({"target": "local", "contract": {"decision_id": "placement-1-2-3"}})
            .to_string(),
    });
    std::fs::write(ws.join(".susi/audit.log"), format!("{entry}\n")).unwrap();
    let found: serde_json::Value = serde_json::from_str(
        &CoreTools::placement_decision(&json!({"decision_id": "placement-1-2-3"}), &ws).unwrap(),
    )
    .unwrap();
    assert_eq!(found["decision"]["target"], "local");
    assert_eq!(found["audit"]["entry_hash"], "abc");
    let not_found = CoreTools::placement_decision(&json!({"decision_id": "placement-9-9-9"}), &ws);
    assert!(not_found
        .unwrap_err()
        .to_string()
        .contains("no INFERENCE_PLACEMENT"));
    let invalid = CoreTools::placement_decision(&json!({"decision_id": "../audit.log"}), &ws);
    assert!(invalid
        .unwrap_err()
        .to_string()
        .contains("invalid placement decision id"));
    assert!(CoreTools::placement_decision(&json!({}), &ws).is_err());
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn runtime_model_tools_require_a_model() {
    let ws = Path::new(".");
    assert!(CoreTools::runtime_models_load(&json!({}), ws).is_err());
    assert!(CoreTools::runtime_models_unload(&json!({"model": ""}), ws).is_err());
}

#[test]
fn runtime_model_tools_return_the_gemi_plane_documents() {
    struct Models;
    impl PlaneHandler for Models {
        fn handle(
            &self,
            topic: &str,
            payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            match topic {
                topics::GEMI_MODELS_LOADED => Ok(json!({"pid": 1, "models": ["m"]})),
                topics::GEMI_MODELS_PRELOAD if payload["model"] == "bad" => {
                    Err("unknown model".to_string())
                }
                other => Ok(json!({"topic": other, "model": payload["model"]})),
            }
        }
    }
    let bus = PlaneBus::global();
    for topic in [
        topics::GEMI_MODELS_LOADED,
        topics::GEMI_MODELS_PRELOAD,
        topics::GEMI_MODELS_UNLOAD,
    ] {
        bus.register(topic, Arc::new(Models));
    }
    let ws = Path::new(".");
    let listed: serde_json::Value =
        serde_json::from_str(&CoreTools::runtime_models_list(&json!({}), ws).unwrap()).unwrap();
    assert_eq!(listed, json!({"pid": 1, "models": ["m"]}));
    let loaded: serde_json::Value =
        serde_json::from_str(&CoreTools::runtime_models_load(&json!({"model": "m"}), ws).unwrap())
            .unwrap();
    assert_eq!(loaded["topic"], topics::GEMI_MODELS_PRELOAD);
    let unloaded: serde_json::Value = serde_json::from_str(
        &CoreTools::runtime_models_unload(&json!({"model": "m"}), ws).unwrap(),
    )
    .unwrap();
    assert_eq!(unloaded["topic"], topics::GEMI_MODELS_UNLOAD);
    let refused = CoreTools::runtime_models_load(&json!({"model": "bad"}), ws).unwrap_err();
    assert!(refused.to_string().contains("unknown model"));
}

#[test]
fn context_graph_tools_serve_stats_and_the_cli_show_document() {
    let ws = temp_dir("graph");
    let stats: serde_json::Value =
        serde_json::from_str(&CoreTools::context_graph_stats(&json!({}), &ws).unwrap()).unwrap();
    assert!(stats["node_count"].is_u64());
    assert!(stats["edge_count"].is_u64());

    let graph = ContextGraph::global();
    graph.record_external_context("test", "first", &json!({}), Some(&ws), None);
    graph.record_external_context("test", "second", &json!({}), Some(&ws), None);
    let shown: serde_json::Value = serde_json::from_str(
        &CoreTools::context_graph_show(
            &json!({"limit": 1, "workspace": ws.display().to_string()}),
            Path::new("."),
        )
        .unwrap(),
    )
    .unwrap();
    let ids = |doc: &serde_json::Value| -> std::collections::BTreeSet<String> {
        doc["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    let all: std::collections::BTreeSet<String> = graph
        .workspace_events(&ws)
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(shown["workspace"], ws.display().to_string());
    assert_eq!(shown["event_count"], 1);
    assert!(ids(&shown).is_subset(&all), "{shown}");
    let default_ws: serde_json::Value =
        serde_json::from_str(&CoreTools::context_graph_show(&json!({}), &ws).unwrap()).unwrap();
    assert_eq!(
        default_ws["event_count"],
        all.len().min(CONTEXT_GRAPH_SHOW_LIMIT)
    );
    assert_eq!(ids(&default_ws), all);

    let relative = CoreTools::context_graph_show(&json!({"workspace": "repo"}), &ws);
    assert!(relative.unwrap_err().to_string().contains("absolute"));
    assert!(CoreTools::context_graph_show(&json!({"limit": "ten"}), &ws).is_err());
    let _ = std::fs::remove_dir_all(&ws);
}
