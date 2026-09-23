//! Plane-bus handler for `agents.*` (meta registry + external executors).

use crate::external::{self, redact, AgentManager, CatalogKind};
use crate::registry::AgentMetaRegistry;
use crate::susi_core::capture::EvidenceSession;
use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use crate::types::AgentProfile;
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
            topics::AGENTS_EXTERNAL_RESOLVE => {
                let id = payload
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or("id required")?;
                let (kind, def) = external::resolve_managed(id).map_err(|e| e.to_string())?;
                Ok(json!({
                    "kind": format!("{kind:?}"),
                    "definition": def,
                }))
            }
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
