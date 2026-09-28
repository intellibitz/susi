//! Plane-bus handler for `agents.*` (meta registry + external executors).

use crate::external::{self, redact, AgentManager, CatalogKind};
use crate::registry::AgentMetaRegistry;
use crate::susi_core::agent_types::AgentProfile;
use crate::susi_core::capture::EvidenceSession;
use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

struct AgentsPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn catalog_kind(payload: &Value) -> CatalogKind {
    match payload
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("execution")
    {
        "framework" => CatalogKind::Framework,
        _ => CatalogKind::Execution,
    }
}

impl PlaneHandler for AgentsPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            topics::AGENTS_EXTERNAL_RUN => {
                let ws = workspace_path(&payload);
                let kind = catalog_kind(&payload);
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                if let Some(run_id) = payload.get("run_id").and_then(|v| v.as_str()) {
                    let record = manager.execute(run_id).map_err(|e| e.to_string())?;
                    return serde_json::to_value(record).map_err(|e| e.to_string());
                }
                let agent = payload
                    .get("agent")
                    .and_then(|v| v.as_str())
                    .ok_or("agent or run_id required")?;
                let prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or("prompt required when starting a run")?;
                let record = manager.start(agent, prompt).map_err(|e| e.to_string())?;
                serde_json::to_value(record).map_err(|e| e.to_string())
            }
            topics::AGENTS_META_LIST => {
                let agents = AgentMetaRegistry::global().list_agents();
                serde_json::to_value(agents).map_err(|e| e.to_string())
            }
            topics::AGENTS_META_REGISTER => {
                let profile: AgentProfile = if let Some(p) = payload.get("profile") {
                    serde_json::from_value(p.clone()).map_err(|e| e.to_string())?
                } else {
                    serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?
                };
                AgentMetaRegistry::global().register_agent(profile);
                Ok(json!({ "ok": true }))
            }
            topics::AGENTS_META_UPDATE_RANK => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("name required")?;
                let delta = payload.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                let source = payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plane_bus");
                AgentMetaRegistry::global().update_rank(name, delta, source);
                Ok(json!({ "ok": true }))
            }
            topics::AGENTS_EXTERNAL_CATALOG => {
                let ws = workspace_path(&payload);
                let kind = catalog_kind(&payload);
                let _ = ws;
                let catalog = external::catalog(kind).map_err(|e| e.to_string())?;
                serde_json::to_value(catalog).map_err(|e| e.to_string())
            }
            topics::AGENTS_EXTERNAL_LIST => {
                let ws = workspace_path(&payload);
                let kind = catalog_kind(&payload);
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                let catalog = external::catalog(kind).map_err(|e| e.to_string())?;
                let rows: Vec<Value> = catalog
                    .into_iter()
                    .map(|agent| {
                        let readiness = manager.adapter(&agent.id).and_then(|a| a.preflight());
                        json!({
                            "agent": agent,
                            "prerequisites_present": readiness.is_ok(),
                            "detail": readiness.unwrap_or_else(|e| e.to_string())
                        })
                    })
                    .collect();
                Ok(json!(rows))
            }
            topics::AGENTS_EXTERNAL_LOGS => {
                let ws = workspace_path(&payload);
                let kind = catalog_kind(&payload);
                let run_id = payload
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .ok_or("run_id required")?;
                let tail = payload
                    .get("tail")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1024 * 1024);
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                let logs = manager
                    .logs(run_id, false, tail)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "text": redact(&logs) }))
            }
            topics::AGENTS_EXTERNAL_RUNS => {
                let ws = workspace_path(&payload);
                let kind = catalog_kind(&payload);
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                let runs = manager.list().map_err(|e| e.to_string())?;
                serde_json::to_value(runs).map_err(|e| e.to_string())
            }
            topics::AGENTS_EXTERNAL_CONTROL => {
                let ws = workspace_path(&payload);
                let id = payload
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or("task_id required")?;
                let action = payload
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("status");
                let kind = catalog_kind(&payload);
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                if action == "logs" {
                    let stderr = payload
                        .get("stderr")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let tail = payload
                        .get("tail")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(65536);
                    let text = manager.logs(id, stderr, tail).map_err(|e| e.to_string())?;
                    return Ok(json!({ "text": redact(&text) }));
                }
                let run = match action {
                    "cancel" => manager.cancel(id),
                    "send" => manager.send(
                        id,
                        payload
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    ),
                    _ if payload
                        .get("refresh")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false) =>
                    {
                        manager.refresh(id)
                    }
                    _ => manager.status(id),
                }
                .map_err(|e| e.to_string())?;
                serde_json::to_value(run).map_err(|e| e.to_string())
            }
            topics::AGENTS_EXTERNAL_MANAGED => {
                let ws = workspace_path(&payload);
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("name required")?;
                let goal = payload
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .ok_or("goal required")?;
                let (kind, def) = external::resolve_managed(name).map_err(|e| e.to_string())?;
                let manager = AgentManager::for_kind(&ws, kind).map_err(|e| e.to_string())?;
                if let Err(e) = manager.adapter(&def.id).and_then(|a| a.preflight()) {
                    return Ok(json!({ "error": format!("[UNAVAILABLE] {name}: {e}") }));
                }
                let result = EvidenceSession::capture_call(
                    &format!("external_peer:{name}"),
                    &json!({"agent": name, "goal": goal}),
                    &ws,
                    || {
                        let run = manager
                            .prepare(&def.id, goal)
                            .and_then(|run| manager.execute(&run.id))
                            .map_err(|e| {
                                crate::susi_core::susi_error::EaiError::process(e.to_string())
                            })?;
                        let output = manager.logs(&run.id, false, 1024 * 1024).map_err(|e| {
                            crate::susi_core::susi_error::EaiError::process(e.to_string())
                        })?;
                        let result = format!("task={} status={:?}\n{}", run.id, run.status, output);
                        if run.status != external::RunStatus::Succeeded {
                            return Err(crate::susi_core::susi_error::EaiError::process(format!(
                                "{}\n{}",
                                result,
                                run.error.unwrap_or_default()
                            )));
                        }
                        Ok(result)
                    },
                )
                .map_err(|e| e.to_string())?;
                Ok(json!({ "text": result }))
            }
            other => Err(format!("agents handler: unhandled topic '{other}'")),
        }
    }
}

pub fn register() {
    PlaneBus::global().register_prefix("agents.", Arc::new(AgentsPlaneHandler));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Isolate the meta registry and the manager's run dirs in a throwaway
    /// instance root; returns (guard, workspace).
    fn isolated() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("susi_plane_test_{}", std::process::id()));
        std::env::set_var("SUSI_HOME", &root);
        let ws = root.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        (guard, ws)
    }

    fn handler() -> AgentsPlaneHandler {
        AgentsPlaneHandler
    }

    #[test]
    fn unhandled_topic_errors() {
        let _g = isolated();
        let err = handler().handle("agents.unknown", json!({})).unwrap_err();
        assert!(err.contains("agents.unknown"));
    }

    #[test]
    fn meta_register_list_and_rank_round_trip() {
        let (_g, _ws) = isolated();
        // Missing required profile fields must surface as Err, not panic.
        assert!(handler()
            .handle(topics::AGENTS_META_REGISTER, json!({}))
            .is_err());
        let profile = json!({
            "name": "plane-test-agent",
            "description": "test",
            "categories": ["test"],
            "semantic_anchors": ["test"],
            "base_rank": 0.5
        });
        let res = handler()
            .handle(topics::AGENTS_META_REGISTER, json!({"profile": profile}))
            .unwrap();
        assert_eq!(res["ok"], true);
        // The payload may also be the bare profile (no "profile" wrapper).
        let profile2 = json!({
            "name": "plane-test-agent-2",
            "description": "test",
            "categories": ["test"],
            "semantic_anchors": ["test"],
            "base_rank": 0.4
        });
        handler()
            .handle(topics::AGENTS_META_REGISTER, profile2)
            .unwrap();
        let list = handler()
            .handle(topics::AGENTS_META_LIST, json!({}))
            .unwrap();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["name"].as_str())
            .collect();
        assert!(names.contains(&"plane-test-agent"));
        // Rank update requires a name; a known name mutates and returns ok.
        assert!(handler()
            .handle(topics::AGENTS_META_UPDATE_RANK, json!({}))
            .is_err());
        let res = handler()
            .handle(
                topics::AGENTS_META_UPDATE_RANK,
                json!({"name": "plane-test-agent", "delta": -0.4}),
            )
            .unwrap();
        assert_eq!(res["ok"], true);
    }

    #[test]
    fn external_run_validates_required_fields() {
        let (_g, ws) = isolated();
        let ws = ws.to_string_lossy().to_string();
        let err = handler()
            .handle(topics::AGENTS_EXTERNAL_RUN, json!({"workspace": ws}))
            .unwrap_err();
        assert!(err.contains("agent or run_id"));
        let err = handler()
            .handle(
                topics::AGENTS_EXTERNAL_RUN,
                json!({"workspace": ws, "agent": "aider"}),
            )
            .unwrap_err();
        assert!(err.contains("prompt"));
        // An unknown run_id takes the execute arm and must fail typed.
        assert!(handler()
            .handle(
                topics::AGENTS_EXTERNAL_RUN,
                json!({"workspace": ws, "run_id": "no-such-run"})
            )
            .is_err());
    }

    #[test]
    fn external_catalog_lists_both_kinds() {
        let (_g, _ws) = isolated();
        for kind in ["framework", "execution"] {
            let res = handler()
                .handle(topics::AGENTS_EXTERNAL_CATALOG, json!({"kind": kind}))
                .unwrap();
            assert!(res.is_array());
        }
    }

    #[test]
    fn external_list_and_runs_respond_against_a_fresh_workspace() {
        let (_g, ws) = isolated();
        let ws = ws.to_string_lossy().to_string();
        let res = handler()
            .handle(topics::AGENTS_EXTERNAL_LIST, json!({"workspace": ws}))
            .unwrap();
        let rows = res.as_array().unwrap();
        assert!(rows
            .iter()
            .all(|r| r.get("prerequisites_present").is_some()));
        let res = handler()
            .handle(topics::AGENTS_EXTERNAL_RUNS, json!({"workspace": ws}))
            .unwrap();
        assert!(res.is_array());
    }

    #[test]
    fn external_logs_requires_run_id() {
        let (_g, ws) = isolated();
        assert!(handler()
            .handle(
                topics::AGENTS_EXTERNAL_LOGS,
                json!({"workspace": ws.to_string_lossy()})
            )
            .is_err());
    }

    #[test]
    fn external_control_requires_task_id_and_dispatches_actions() {
        let (_g, ws) = isolated();
        let ws = ws.to_string_lossy().to_string();
        let err = handler()
            .handle(topics::AGENTS_EXTERNAL_CONTROL, json!({"workspace": ws}))
            .unwrap_err();
        assert!(err.contains("task_id"));
        // Unknown task ids fail typed across every action arm.
        for payload in [
            json!({"workspace": ws, "task_id": "no-such", "action": "logs"}),
            json!({"workspace": ws, "task_id": "no-such", "action": "cancel"}),
            json!({"workspace": ws, "task_id": "no-such", "action": "send"}),
            json!({"workspace": ws, "task_id": "no-such", "action": "status", "refresh": true}),
            json!({"workspace": ws, "task_id": "no-such"}),
        ] {
            assert!(handler()
                .handle(topics::AGENTS_EXTERNAL_CONTROL, payload)
                .is_err());
        }
    }

    #[test]
    fn managed_requires_name_and_goal() {
        let (_g, ws) = isolated();
        let ws = ws.to_string_lossy().to_string();
        assert!(handler()
            .handle(topics::AGENTS_EXTERNAL_MANAGED, json!({"workspace": ws}))
            .is_err());
        assert!(handler()
            .handle(
                topics::AGENTS_EXTERNAL_MANAGED,
                json!({"workspace": ws, "name": "nope"})
            )
            .is_err());
        // Unknown managed names fail at resolve, not at spawn.
        assert!(handler()
            .handle(
                topics::AGENTS_EXTERNAL_MANAGED,
                json!({"workspace": ws, "name": "nope", "goal": "x"})
            )
            .is_err());
    }

    #[test]
    fn register_wires_the_prefix() {
        let _g = isolated();
        register();
        let res = PlaneBus::global().request(topics::AGENTS_META_LIST, json!({}));
        assert!(res.is_ok());
    }
}
