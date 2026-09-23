//! Plane-bus handler for all `gawd.*` topics.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use susi_core::plane_bus::topics;
use susi_core::plane_bus::{PlaneBus, PlaneHandler};
use susi_error::EaiResult;

struct GawdPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn eai_to_string<T>(r: EaiResult<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

impl PlaneHandler for GawdPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        crate::init_hooks();
        match topic {
            topics::GAWD_SOLVE => {
                let intent = payload.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let version = payload
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or(env!("CARGO_PKG_VERSION"));
                let text = crate::ama::SusiMasterAgent::new().solve_clean(intent, &ws, version);
                Ok(json!({ "text": text }))
            }
            topics::GAWD_SANITIZE => {
                let input = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
                match crate::ama::SusiMasterAgent::sanitize_input(input) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_AUDIT_ACTION => {
                let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                let detail = payload.get("detail").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                if let Err(e) = crate::safety::SafetyDetector::audit_action(tool, detail, &ws) {
                    return Ok(json!({ "error": e.to_string() }));
                }
                match crate::security::SecurityDetector::audit_action(tool, detail, &ws) {
                    Ok(()) => Ok(json!({ "ok": true })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_PATCH => {
                let ws = workspace_path(&payload);
                let trust = payload
                    .get("trust_level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("manual");
                let request: crate::patch_cycle::PatchRequest = if payload.get("files").is_some() {
                    serde_json::from_value(payload.clone())
                        .map_err(|e| format!("invalid patch payload: {e}"))?
                } else {
                    let request_json = payload
                        .get("request_json")
                        .and_then(|v| v.as_str())
                        .ok_or("request_json or files required")?;
                    serde_json::from_str(request_json)
                        .map_err(|e| format!("invalid patch request JSON: {e}"))?
                };
                match crate::patch_cycle::apply_patch_cycle(&ws, &request, trust) {
                    Ok(outcome) => serde_json::to_value(outcome).map_err(|e| e.to_string()),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GAWD_BLOAT => {
                let ws = workspace_path(&payload);
                let report = eai_to_string(crate::bloat_audit::BloatAuditor::audit_workspace(&ws))?;
                Ok(json!({
                    "text": crate::bloat_audit::BloatAuditor::render_report(&report)
                }))
            }
            topics::GAWD_IDENTITY => {
                let ws = workspace_path(&payload);
                Ok(json!({ "text": susi_gawd_agents::self_core::identity_report(&ws) }))
            }
            topics::GAWD_REASON_AUDIT => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(
                    crate::reason_trainer::ReasoningTrainer::audit_reasoning_substrate(&ws),
                )?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_SELF_VALIDATE => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(
                    crate::self_validation::execute_autonomous_self_validation(&ws),
                )?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_TRAIN_REFLEX => {
                let ws = workspace_path(&payload);
                let text = eai_to_string(crate::reflex_trainer::ReflexTrainer::force_train(&ws))?;
                Ok(json!({ "text": text }))
            }
            topics::GAWD_CAPABILITY_GAP => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let text = match crate::reflex_synth::ReflexSynthesizer::synthesize_wasm_reflex(
                    name, &ws,
                )
                {
                    Ok(wasm_path) => format!(
                        "[HOT_PATCH] Synthesized WASI reflex for '{name}' at {wasm_path}. Retry as 'reflex_{name}'."
                    ),
                    Err(e) => format!(
                        "[CAPABILITY_GAP] '{name}' unresolved: reflex synthesis failed ({e})."
                    ),
                };
                Ok(json!({ "text": text }))
            }
            topics::GAWD_LOCK_BROADCAST => {
                let resource_id = payload
                    .get("resource_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(json!({
                    "ok": crate::amas::SusiSupervisor::broadcast_lock_request(resource_id)
                }))
            }
            topics::GAWD_SCHEDULER_RECENT => {
                let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let mut decisions =
                    susi_gawd_agents::scheduler::MissionScheduler::recent_decisions();
                decisions.truncate(limit);
                serde_json::to_value(decisions).map_err(|e| e.to_string())
            }
            other => Err(format!("gawd handler: unhandled topic '{other}'")),
        }
    }
}

pub fn register() {
    PlaneBus::global().register_prefix("gawd.", Arc::new(GawdPlaneHandler));
}
