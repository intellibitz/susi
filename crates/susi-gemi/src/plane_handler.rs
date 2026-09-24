//! Plane-bus handler for all `gemi.*` topics (real GEMI / models APIs).

use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{HardwareProfileDto, PlaneBus, PlaneHandler};
use crate::susi_core::registry::CapabilityRegistry;
use crate::susi_error::EaiResult;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::{GemiEngine, MissionPlanner};
use crate::hardware::HardwareProfiler;
use crate::http_provider;
use crate::models::ModelManager;
use crate::pulse::SusiPulse;
use crate::routing::InferenceRouter;
use crate::telemetry;
use susi_gemi_models::coding_models::CodingModelManager;
use susi_gemi_models::intent::{IntentCategory, IntentClassifier, TaskComplexity};

struct GemiPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn path_field(payload: &Value, key: &str) -> PathBuf {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn eai_to_string<T>(r: EaiResult<T>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

fn profile_dto(p: crate::hardware::HardwareProfile) -> HardwareProfileDto {
    HardwareProfileDto {
        cpus: p.cpus,
        cpu_brand: p.cpu_brand,
        gpu_info: p.gpu_info,
        ram_gb: p.ram_gb,
        available_ram_gb: p.available_ram_gb,
        gpu_vram_gb: p.gpu_vram_gb,
        swap_gb: p.swap_gb,
        nvme_active: p.nvme_active,
        acceleration_active: p.acceleration_active,
        native_acceleration: p.native_acceleration,
        os_info: p.os_info,
        arch: p.arch,
        disk_gb: p.disk_gb,
        disk_usage_pct: p.disk_usage_pct,
        load_avg: p.load_avg,
        uptime: p.uptime,
        hostname: p.hostname,
    }
}

fn parse_intent(s: &str) -> Option<IntentCategory> {
    match s.to_lowercase().as_str() {
        "coding" => Some(IntentCategory::Coding),
        "mathematics" | "math" => Some(IntentCategory::Mathematics),
        "reasoning" => Some(IntentCategory::Reasoning),
        "creative" => Some(IntentCategory::Creative),
        "general" | "" => Some(IntentCategory::General),
        _ => None,
    }
}

fn parse_complexity(s: &str) -> Option<TaskComplexity> {
    match s {
        "Trivial" => Some(TaskComplexity::Trivial),
        "Simple" => Some(TaskComplexity::Simple),
        "Moderate" => Some(TaskComplexity::Moderate),
        "Complex" => Some(TaskComplexity::Complex),
        "VeryComplex" => Some(TaskComplexity::VeryComplex),
        _ => None,
    }
}

impl PlaneHandler for GemiPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            topics::GEMI_HARDWARE_PROFILE => {
                let dto = profile_dto(HardwareProfiler::get_profile());
                serde_json::to_value(dto).map_err(|e| e.to_string())
            }
            topics::GEMI_HARDWARE_OOM => {
                Ok(json!({ "oom": HardwareProfiler::check_oom_critical() }))
            }
            topics::GEMI_HARDWARE_CAPS => {
                Ok(json!({ "caps": HardwareProfiler::get_caps_string() }))
            }
            topics::GEMI_MODELS_LIST => {
                let ws = workspace_path(&payload);
                let models = ModelManager::list_models(&ws);
                serde_json::to_value(models).map_err(|e| e.to_string())
            }
            topics::GEMI_MODELS_SELECT => {
                if let Some(set) = payload.get("set").and_then(|v| v.as_str()) {
                    let msg = ModelManager::set_selected_model(set).map_err(|e| e.to_string())?;
                    Ok(json!({ "text": msg }))
                } else {
                    let intent = payload
                        .get("intent")
                        .and_then(|v| v.as_str())
                        .and_then(parse_intent);
                    Ok(json!({
                        "model": ModelManager::get_selected_model(intent)
                    }))
                }
            }
            topics::GEMI_MODELS_ACTIVE => {
                let intent = payload
                    .get("intent")
                    .and_then(|v| v.as_str())
                    .and_then(parse_intent);
                let (engine, model) = ModelManager::get_active_engine_and_model(intent);
                Ok(json!({ "engine": engine, "model": model }))
            }
            topics::GEMI_MODELS_PATH => {
                let model_id = payload
                    .get("model_id")
                    .and_then(|v| v.as_str())
                    .ok_or("model_id required")?;
                Ok(json!({
                    "path": ModelManager::get_model_path(model_id)
                        .map(|p| p.display().to_string())
                }))
            }
            topics::GEMI_MODELS_VERIFY => {
                let ws = workspace_path(&payload);
                let results = ModelManager::verify_local_models(&ws);
                let items: Vec<Value> = results
                    .into_iter()
                    .map(|r| {
                        json!({
                            "model_id": r.model_id,
                            "path": r.path,
                            "file_size_bytes": r.file_size_bytes,
                            "file_size_formatted": r.file_size_formatted,
                            "is_valid_gguf": r.is_valid_gguf,
                            "magic_header": r.magic_header,
                            "test_inference_status": r.test_inference_status,
                            "latency_ms": r.latency_ms,
                            "checksum_verified": r.checksum_verified,
                        })
                    })
                    .collect();
                Ok(json!(items))
            }
            topics::GEMI_MODELS_INSTALL => {
                let target = payload
                    .get("target")
                    .and_then(|v| v.as_str())
                    .ok_or("target required")?;
                Ok(json!({ "text": ModelManager::install_model(target) }))
            }
            topics::GEMI_MODELS_ENSURE => {
                let ws = workspace_path(&payload);
                match ModelManager::ensure_hardware_optimal_models(&ws) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GEMI_MODELS_SELECT_MIN => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let min_c = payload
                    .get("min_complexity")
                    .and_then(|v| v.as_str())
                    .and_then(parse_complexity);
                Ok(json!({
                    "model": ModelManager::get_selected_model_for_request_with_min_complexity(
                        prompt,
                        None,
                        min_c,
                    )
                }))
            }
            topics::GEMI_MODELS_SCAN => {
                let dir = path_field(&payload, "global_dir");
                match ModelManager::deep_scan_home_and_register(&dir) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GEMI_INFER_GENERATE => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                Ok(json!({
                    "text": GemiEngine::generate_reasoning(prompt, &ws)
                }))
            }
            topics::GEMI_INFER_GENERATE_DEEP => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let min_c = payload
                    .get("min_complexity")
                    .and_then(|v| v.as_str())
                    .and_then(parse_complexity);
                let text = if let Some(c) = min_c {
                    GemiEngine::generate_reasoning_deep_with_min_complexity(prompt, &ws, Some(c))
                } else {
                    GemiEngine::generate_reasoning_deep(prompt, &ws)
                };
                Ok(json!({ "text": text }))
            }
            topics::GEMI_INFER_GENERATE_DEEP_MODEL => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let model = payload
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                Ok(json!({
                    "text": GemiEngine::generate_reasoning_deep_with_model(prompt, &ws, model)
                }))
            }
            topics::GEMI_INFER_VERIFY => {
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                match GemiEngine::verify_axiomatic_alignment(text, &ws) {
                    Ok(t) => Ok(json!({ "text": t })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            topics::GEMI_INFER_STREAM => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let stream_id = payload
                    .get("stream_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let model = payload.get("model").and_then(|v| v.as_str());
                let bus = PlaneBus::global();
                let emit = |chunk: String| {
                    if !stream_id.is_empty() {
                        bus.stream_emit(stream_id, json!(chunk));
                    }
                };
                // The serving backend is reported as a structured chunk
                // before any content — text consumers skip non-string
                // chunks; the OpenAI SSE layer uses it to label frames with
                // the actual generator rather than the requested model.
                let meta = |name: &str| {
                    if !stream_id.is_empty() {
                        bus.stream_emit(stream_id, json!({ "susi_meta": { "provider": name } }));
                    }
                };
                let text =
                    GemiEngine::generate_reasoning_stream_meta(prompt, &ws, &emit, model, &meta);
                Ok(json!({ "text": text }))
            }
            topics::GEMI_INFER_EMBED => {
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let want = payload.get("model").and_then(|v| v.as_str());
                match GemiEngine::embed_text(text, want) {
                    Ok(v) => Ok(json!({ "embedding": v })),
                    Err(e) => Ok(json!({ "error": e })),
                }
            }
            topics::GEMI_PLAN_PARTITION => {
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let plan = eai_to_string(MissionPlanner::partition_mission(goal, &ws))?;
                Ok(json!({ "goals": plan.goals }))
            }
            topics::GEMI_PLAN_MISSION => {
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let plan = eai_to_string(MissionPlanner::plan_mission(goal, &ws))?;
                Ok(json!({ "goals": plan.goals }))
            }
            topics::GEMI_PLAN_REFINE => {
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                let plan = payload.get("plan").cloned().unwrap_or(json!({}));
                let blackboard = plan.to_string();
                let ws = workspace_path(&payload);
                let refined = eai_to_string(MissionPlanner::refine_plan(goal, &blackboard, &ws))?;
                Ok(json!({ "goals": refined.goals }))
            }
            topics::GEMI_INTENT_CLASSIFY => {
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                let intent = IntentClassifier::classify(goal);
                Ok(json!({ "intent": format!("{intent:?}").to_lowercase() }))
            }
            topics::GEMI_ALPHA_PROJECTION => {
                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let vec = crate::alpha::SusiAlphaModel::semantic_centroid_projection(text, None)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "vector": vec }))
            }
            topics::GEMI_ALPHA_TRAIN => {
                let ws = workspace_path(&payload);
                let global_dir = crate::susi_paths::SusiDirs::config_dir();
                let _ = ws;
                let text = crate::alpha::SusiAlphaModel::train_on_staged_data(&global_dir)
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "text": text }))
            }
            topics::GEMI_TELEMETRY_SAMPLE => {
                let snap = telemetry::sample();
                serde_json::to_value(snap).map_err(|e| e.to_string())
            }
            topics::GEMI_CLOUD_APPLY_ENV => {
                http_provider::apply_cloud_env_file();
                Ok(json!({ "ok": true }))
            }
            topics::GEMI_CLOUD_REGISTER => {
                http_provider::register_configured_cloud_endpoints(CapabilityRegistry::global());
                Ok(json!({ "ok": true }))
            }
            topics::GEMI_CLOUD_FAILOVER => {
                let order = InferenceRouter::cloud_failover_order(CapabilityRegistry::global());
                // Failover order means *currently viable* order: providers
                // inside their post-failure cooldown are skipped here so
                // consumers (swarm recovery, endpoint cascades) do not
                // re-probe a dead vendor once per mission. `cooled` is
                // reported for observability.
                let (order, cooled): (Vec<String>, Vec<String>) = order
                    .into_iter()
                    .partition(|name| !InferenceRouter::provider_cooled(name));
                Ok(json!({ "order": order, "cooled": cooled }))
            }
            topics::GEMI_PROVIDER_FAILURE => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let error = payload.get("error").and_then(|v| v.as_str()).unwrap_or("");
                if !name.is_empty() {
                    InferenceRouter::record_failure(name, error);
                }
                Ok(json!({ "ok": true }))
            }
            topics::GEMI_PROVIDER_SUCCESS => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if !name.is_empty() {
                    InferenceRouter::record_provider_success(name);
                }
                Ok(json!({ "ok": true }))
            }
            topics::GEMI_PULSE_REASON => {
                let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let ws = workspace_path(&payload);
                let text = SusiPulse::reason(prompt, &ws).map_err(|e| e.to_string())?;
                Ok(json!({ "text": text }))
            }
            topics::GEMI_CODING_CATALOG => match CodingModelManager::catalog() {
                Ok(catalog) => serde_json::to_value(catalog).map_err(|e| e.to_string()),
                Err(e) => Ok(json!({ "error": e.to_string() })),
            },
            other => Err(format!("gemi handler: unhandled topic '{other}'")),
        }
    }
}

pub fn register() {
    PlaneBus::global().register_prefix("gemi.", Arc::new(GemiPlaneHandler));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The swarm recovery loop reports provider attempts through the bus —
    /// the failure topic must feed the shared cooldown classifier and the
    /// success topic must clear it, or dead vendors get re-probed forever.
    #[test]
    fn provider_failure_and_success_topics_drive_cooldown() {
        let _env = crate::engines::env_test_lock();
        let path =
            std::env::temp_dir().join(format!("susi-cd-handler-{}.json", std::process::id()));
        // SAFETY: test-only env override, serialized by env_test_lock and
        // restored before this test returns.
        unsafe {
            std::env::set_var("SUSI_COOLDOWNS_FILE", &path);
        }
        let handler = GemiPlaneHandler;
        let name = format!("vendhandler{}-model-a", std::process::id());
        let sibling = name.replace("model-a", "model-b");

        let v = handler
            .handle(
                topics::GEMI_PROVIDER_FAILURE,
                json!({ "name": name, "error": "HTTP 429: rate limited" }),
            )
            .expect("failure topic must be handled");
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert!(InferenceRouter::provider_cooled(&name));
        // 429 is credential-scoped — the sibling cools too.
        assert!(InferenceRouter::provider_cooled(&sibling));

        let v = handler
            .handle(topics::GEMI_PROVIDER_SUCCESS, json!({ "name": name }))
            .expect("success topic must be handled");
        assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
        assert!(!InferenceRouter::provider_cooled(&name));
        assert!(!InferenceRouter::provider_cooled(&sibling));

        // Empty payloads are tolerated, never panic.
        assert!(handler
            .handle(topics::GEMI_PROVIDER_FAILURE, json!({}))
            .is_ok());
        unsafe {
            std::env::remove_var("SUSI_COOLDOWNS_FILE");
        }
        let _ = std::fs::remove_file(&path);
    }
}
