//! In-process **plane message bus** — the only allowed control plane between
//! feature crates (gawd / gemi / gmcp / tools / agents / server).
//!
//! Feature planes must not depend on each other in Cargo.toml. They talk only
//! through this bus (and shared foundation: paths / error / core / config /
//! sandbox / native). Composition roots (`susi-daemon`, CLI) register handlers
//! and may still link every plane.

use crate::plane_bus_ipc::IpcPlaneBus;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Topic namespaces owned by each feature plane.
pub mod topics {
    pub const GEMI_HARDWARE_PROFILE: &str = "gemi.hardware.profile";
    pub const GEMI_HARDWARE_OOM: &str = "gemi.hardware.oom";
    pub const GEMI_HARDWARE_CAPS: &str = "gemi.hardware.caps";
    pub const GEMI_MODELS_LIST: &str = "gemi.models.list";
    pub const GEMI_MODELS_SELECT: &str = "gemi.models.select";
    pub const GEMI_MODELS_ACTIVE: &str = "gemi.models.active";
    pub const GEMI_MODELS_PATH: &str = "gemi.models.path";
    pub const GEMI_MODELS_VERIFY: &str = "gemi.models.verify";
    pub const GEMI_MODELS_INSTALL: &str = "gemi.models.install";
    pub const GEMI_MODELS_ENSURE: &str = "gemi.models.ensure";
    pub const GEMI_MODELS_SCAN: &str = "gemi.models.scan";
    pub const GEMI_INFER_GENERATE: &str = "gemi.infer.generate";
    pub const GEMI_INFER_GENERATE_DEEP: &str = "gemi.infer.generate_deep";
    pub const GEMI_INFER_GENERATE_DEEP_MODEL: &str = "gemi.infer.generate_deep_model";
    pub const GEMI_INFER_VERIFY: &str = "gemi.infer.verify";
    pub const GEMI_INFER_STREAM: &str = "gemi.infer.stream";
    pub const GEMI_PLAN_PARTITION: &str = "gemi.plan.partition";
    pub const GEMI_PLAN_MISSION: &str = "gemi.plan.mission";
    pub const GEMI_PLAN_REFINE: &str = "gemi.plan.refine";
    pub const GEMI_INTENT_CLASSIFY: &str = "gemi.intent.classify";
    pub const GEMI_ALPHA_PROJECTION: &str = "gemi.alpha.projection";
    pub const GEMI_ALPHA_TRAIN: &str = "gemi.alpha.train";
    pub const GEMI_TELEMETRY_SAMPLE: &str = "gemi.telemetry.sample";
    pub const GEMI_CLOUD_APPLY_ENV: &str = "gemi.cloud.apply_env";
    pub const GEMI_CLOUD_REGISTER: &str = "gemi.cloud.register";
    pub const GEMI_CLOUD_FAILOVER: &str = "gemi.cloud.failover";
    pub const GEMI_PULSE_REASON: &str = "gemi.pulse.reason";
    pub const GEMI_CODING_CATALOG: &str = "gemi.coding.catalog";

    pub const GAWD_SOLVE: &str = "gawd.mission.solve";
    pub const GAWD_SANITIZE: &str = "gawd.mission.sanitize";
    pub const GAWD_AUDIT_ACTION: &str = "gawd.audit.action";
    pub const GAWD_PATCH: &str = "gawd.patch.apply";
    pub const GAWD_BLOAT: &str = "gawd.admin.bloat";
    pub const GAWD_IDENTITY: &str = "gawd.admin.identity";
    pub const GAWD_REASON_AUDIT: &str = "gawd.admin.reason_audit";
    pub const GAWD_SELF_VALIDATE: &str = "gawd.admin.self_validate";
    pub const GAWD_TRAIN_REFLEX: &str = "gawd.admin.train_reflex";
    pub const GAWD_CAPABILITY_GAP: &str = "gawd.admin.capability_gap";
    pub const GAWD_LOCK_BROADCAST: &str = "gawd.admin.lock_broadcast";
    pub const GAWD_SCHEDULER_RECENT: &str = "gawd.scheduler.recent";
    /// Verified cluster roster (node_id ↔ address) for commit-ledger
    /// anti-entropy: a receiver that detects a seq gap resolves the
    /// coordinator's address here, then pulls the missing records.
    pub const GAWD_CLUSTER_PEERS: &str = "gawd.cluster.peers";

    pub const TOOLS_EXISTS: &str = "tools.registry.exists";
    pub const TOOLS_EXECUTE: &str = "tools.registry.execute";
    pub const TOOLS_AUTO_LINK: &str = "tools.registry.auto_link";
    pub const TOOLS_SCOUT_REMOTES: &str = "tools.mcp.scout_remotes";
    pub const TOOLS_REMOTE_EXECUTE: &str = "tools.mcp.remote_execute";
    pub const TOOLS_LIST: &str = "tools.registry.list";
    pub const TOOLS_LEADING_LIST: &str = "tools.leading.list";
    pub const TOOLS_LEADING_GET: &str = "tools.leading.get";
    pub const TOOLS_LEADING_REMOVE: &str = "tools.leading.remove";

    pub const AGENTS_EXTERNAL_MANAGED: &str = "agents.external.managed";
    pub const AGENTS_EXTERNAL_CATALOG: &str = "agents.external.catalog";
    pub const AGENTS_EXTERNAL_LOGS: &str = "agents.external.logs";
    pub const AGENTS_EXTERNAL_RUNS: &str = "agents.external.runs";
    pub const AGENTS_EXTERNAL_LIST: &str = "agents.external.list";
    pub const AGENTS_EXTERNAL_CONTROL: &str = "agents.external.control";
    pub const GEMI_MODELS_SELECT_MIN: &str = "gemi.models.select_min";
    pub const GEMI_CODING_CONFIGURE: &str = "gemi.coding.configure";

    pub const AGENTS_EXTERNAL_RESOLVE: &str = "agents.external.resolve";
    pub const AGENTS_EXTERNAL_RUN: &str = "agents.external.run";
    pub const AGENTS_META_LIST: &str = "agents.meta.list";
    pub const AGENTS_META_REGISTER: &str = "agents.meta.register";
    pub const AGENTS_META_UPDATE_RANK: &str = "agents.meta.update_rank";
}

/// Handler for one or more topic prefixes (exact topic match first).
pub trait PlaneHandler: Send + Sync {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String>;
}

/// Process-wide plane bus — facade over [`IpcPlaneBus`]. Every method
/// delegates to the IPC backend so registrations and requests made through
/// this `global()` are visible to all vendored `susi_core` copies in the
/// same process (shared `<cache>/bus/<pid>/` rendezvous).
pub struct PlaneBus {
    inner: Arc<IpcPlaneBus>,
}

impl PlaneBus {
    pub fn global() -> &'static PlaneBus {
        static BUS: OnceLock<PlaneBus> = OnceLock::new();
        BUS.get_or_init(|| PlaneBus {
            // Shared with this crate's registry_ipc/broker/etc. so one
            // listener serves all modules.
            inner: IpcPlaneBus::global(),
        })
    }

    pub fn register(&self, topic: &str, handler: Arc<dyn PlaneHandler>) {
        self.inner.register(topic, handler);
    }

    pub fn register_prefix(&self, prefix: &str, handler: Arc<dyn PlaneHandler>) {
        self.inner.register_prefix(prefix, handler);
    }

    pub fn request(&self, topic: &str, payload: Value) -> Result<Value, String> {
        self.inner.request(topic, payload)
    }

    pub fn publish(&self, topic: &str, payload: Value) {
        self.inner.publish(topic, payload);
    }

    /// Allocate a stream channel; handler emits via [`Self::stream_emit`].
    pub fn open_stream(&self) -> (String, flume::Receiver<Value>) {
        self.inner.open_stream()
    }

    pub fn stream_emit(&self, stream_id: &str, chunk: Value) {
        self.inner.stream_emit(stream_id, chunk);
    }

    pub fn stream_close(&self, stream_id: &str) {
        self.inner.stream_close(stream_id);
    }

    pub fn is_wired(&self, topic: &str) -> bool {
        self.inner.is_wired(topic)
    }
}

// ── Shared DTOs (serde; planes convert at the boundary) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HardwareProfileDto {
    pub cpus: usize,
    pub cpu_brand: String,
    pub gpu_info: String,
    pub ram_gb: usize,
    pub available_ram_gb: usize,
    pub gpu_vram_gb: usize,
    pub swap_gb: usize,
    pub nvme_active: bool,
    pub acceleration_active: bool,
    pub native_acceleration: String,
    pub os_info: String,
    pub arch: String,
    pub disk_gb: usize,
    pub disk_usage_pct: u8,
    pub load_avg: String,
    pub uptime: String,
    pub hostname: String,
}

fn req(topic: &str, payload: Value) -> Result<Value, String> {
    PlaneBus::global().request(topic, payload)
}

fn req_ok(topic: &str, payload: Value) -> Value {
    req(topic, payload).unwrap_or_else(|e| json!({ "error": e }))
}

// ── Gemi facade (call sites use these instead of `susi_gemi::*`) ──────────

pub mod gemi {
    use super::*;

    pub struct HardwareProfiler;

    impl HardwareProfiler {
        pub fn get_profile() -> HardwareProfileDto {
            let v = req_ok(topics::GEMI_HARDWARE_PROFILE, json!({}));
            serde_json::from_value(v).unwrap_or_default()
        }

        pub fn check_oom_critical() -> bool {
            req_ok(topics::GEMI_HARDWARE_OOM, json!({}))
                .get("oom")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        }

        pub fn get_caps_string() -> String {
            req_ok(topics::GEMI_HARDWARE_CAPS, json!({}))
                .get("caps")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }

        /// Candle device is plane-local; bus returns a label only.
        pub fn get_candle_device_label() -> String {
            Self::get_profile().native_acceleration
        }
    }

    pub struct ModelManager;

    impl ModelManager {
        pub fn list_models(workspace: &Path) -> Value {
            req_ok(
                topics::GEMI_MODELS_LIST,
                json!({ "workspace": workspace.display().to_string() }),
            )
        }

        pub fn get_selected_model(workspace: Option<&Path>) -> Option<String> {
            let ws = workspace
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            req_ok(topics::GEMI_MODELS_SELECT, json!({ "workspace": ws }))
                .get("model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }

        pub fn get_active_engine_and_model(intent: Option<&str>) -> (String, String) {
            let v = req_ok(
                topics::GEMI_MODELS_ACTIVE,
                json!({ "intent": intent.unwrap_or("") }),
            );
            (
                v.get("engine")
                    .and_then(|x| x.as_str())
                    .unwrap_or("local")
                    .to_string(),
                v.get("model")
                    .and_then(|x| x.as_str())
                    .unwrap_or("default")
                    .to_string(),
            )
        }

        pub fn get_model_path(model_id: &str) -> Option<PathBuf> {
            req_ok(topics::GEMI_MODELS_PATH, json!({ "model_id": model_id }))
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
        }

        pub fn verify_local_models(workspace: &Path) -> Value {
            req_ok(
                topics::GEMI_MODELS_VERIFY,
                json!({ "workspace": workspace.display().to_string() }),
            )
        }

        pub fn install_model(url_or_id: &str) -> Value {
            req_ok(topics::GEMI_MODELS_INSTALL, json!({ "target": url_or_id }))
        }

        pub fn ensure_hardware_optimal_models(workspace: &Path) -> Value {
            req_ok(
                topics::GEMI_MODELS_ENSURE,
                json!({ "workspace": workspace.display().to_string() }),
            )
        }

        pub fn deep_scan_home_and_register(global_dir: &Path) -> Value {
            req_ok(
                topics::GEMI_MODELS_SCAN,
                json!({ "global_dir": global_dir.display().to_string() }),
            )
        }

        pub fn set_selected_model(model: &str) -> Value {
            req_ok(topics::GEMI_MODELS_SELECT, json!({ "set": model }))
        }

        pub fn get_selected_model_for_request_with_min_complexity(
            prompt: &str,
            workspace: Option<&Path>,
            min_complexity: Option<&str>,
        ) -> Option<String> {
            let ws = workspace
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            req_ok(
                topics::GEMI_MODELS_SELECT_MIN,
                json!({
                    "prompt": prompt,
                    "workspace": ws,
                    "min_complexity": min_complexity.unwrap_or("")
                }),
            )
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        }

        pub fn list_models_len(workspace: &Path) -> usize {
            Self::list_models(workspace)
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
        }
    }

    pub struct GemiEngine;

    impl GemiEngine {
        pub fn generate_reasoning(prompt: &str, workspace: &Path) -> String {
            req_ok(
                topics::GEMI_INFER_GENERATE,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string()
                }),
            )
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
        }

        pub fn generate_reasoning_stream(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
        ) -> String {
            let bus = PlaneBus::global();
            let (stream_id, rx) = bus.open_stream();
            let result = req(
                topics::GEMI_INFER_STREAM,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string(),
                    "stream_id": stream_id,
                }),
            );
            while let Ok(chunk) = rx.try_recv() {
                if let Some(s) = chunk.as_str() {
                    on_chunk(s.to_string());
                } else if let Some(s) = chunk.get("text").and_then(|v| v.as_str()) {
                    on_chunk(s.to_string());
                }
            }
            // Drain remaining after handler returns
            while let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(10)) {
                if let Some(s) = chunk.as_str() {
                    on_chunk(s.to_string());
                } else if let Some(s) = chunk.get("text").and_then(|v| v.as_str()) {
                    on_chunk(s.to_string());
                }
            }
            bus.stream_close(&stream_id);
            match result {
                Ok(v) => v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                Err(e) => format!("[plane bus] {e}"),
            }
        }

        pub fn generate_reasoning_deep(prompt: &str, workspace: &Path) -> String {
            req_ok(
                topics::GEMI_INFER_GENERATE_DEEP,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string()
                }),
            )
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
        }

        pub fn generate_reasoning_deep_with_model(
            prompt: &str,
            workspace: &Path,
            model: &str,
        ) -> String {
            req_ok(
                topics::GEMI_INFER_GENERATE_DEEP_MODEL,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string(),
                    "model": model
                }),
            )
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
        }

        pub fn generate_reasoning_deep_with_min_complexity(
            prompt: &str,
            workspace: &Path,
            complexity: &str,
        ) -> String {
            req_ok(
                topics::GEMI_INFER_GENERATE_DEEP,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string(),
                    "min_complexity": complexity
                }),
            )
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
        }

        pub fn verify_axiomatic_alignment(text: &str, workspace: &Path) -> Result<String, String> {
            let v = req(
                topics::GEMI_INFER_VERIFY,
                json!({
                    "text": text,
                    "workspace": workspace.display().to_string()
                }),
            )?;
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                return Err(err.to_string());
            }
            Ok(v.get("text")
                .and_then(|x| x.as_str())
                .unwrap_or(text)
                .to_string())
        }
    }

    pub struct MissionPlanner;

    impl MissionPlanner {
        pub fn partition_mission(goal: &str, workspace: &Path) -> Result<Value, String> {
            req(
                topics::GEMI_PLAN_PARTITION,
                json!({
                    "goal": goal,
                    "workspace": workspace.display().to_string()
                }),
            )
        }

        pub fn plan_mission(goal: &str, workspace: &Path) -> Result<Value, String> {
            req(
                topics::GEMI_PLAN_MISSION,
                json!({
                    "goal": goal,
                    "workspace": workspace.display().to_string()
                }),
            )
        }

        pub fn refine_plan(goal: &str, plan: &Value, workspace: &Path) -> Result<Value, String> {
            req(
                topics::GEMI_PLAN_REFINE,
                json!({
                    "goal": goal,
                    "plan": plan,
                    "workspace": workspace.display().to_string()
                }),
            )
        }
    }

    pub struct IntentClassifier;

    impl IntentClassifier {
        pub fn classify(goal: &str) -> String {
            req_ok(topics::GEMI_INTENT_CLASSIFY, json!({ "goal": goal }))
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or("general")
                .to_string()
        }
    }

    pub struct SusiAlphaModel;

    impl SusiAlphaModel {
        pub fn semantic_centroid_projection(
            text: &str,
            workspace: &Path,
        ) -> Result<Vec<f32>, String> {
            let v = req(
                topics::GEMI_ALPHA_PROJECTION,
                json!({
                    "text": text,
                    "workspace": workspace.display().to_string()
                }),
            )?;
            serde_json::from_value(v.get("vector").cloned().unwrap_or(json!([])))
                .map_err(|e| e.to_string())
        }

        pub fn train_on_staged_data(workspace: &Path) -> Result<String, String> {
            let v = req(
                topics::GEMI_ALPHA_TRAIN,
                json!({ "workspace": workspace.display().to_string() }),
            )?;
            Ok(v.get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string())
        }
    }

    pub fn sample_telemetry() -> Value {
        req_ok(topics::GEMI_TELEMETRY_SAMPLE, json!({}))
    }

    pub fn apply_cloud_env_file() {
        let _ = req(topics::GEMI_CLOUD_APPLY_ENV, json!({}));
    }

    pub fn register_configured_cloud_endpoints() -> Value {
        req_ok(topics::GEMI_CLOUD_REGISTER, json!({}))
    }

    pub fn cloud_failover_order() -> Value {
        req_ok(topics::GEMI_CLOUD_FAILOVER, json!({}))
    }

    pub fn coding_catalog() -> Value {
        req_ok(topics::GEMI_CODING_CATALOG, json!({}))
    }

    pub fn pulse_reason(prompt: &str, workspace: &Path) -> String {
        req_ok(
            topics::GEMI_PULSE_REASON,
            json!({
                "prompt": prompt,
                "workspace": workspace.display().to_string()
            }),
        )
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
    }
}

// ── Gawd facade ───────────────────────────────────────────────────────────

pub mod gawd {
    use super::*;

    pub fn solve_mission(intent: &str, workspace: &Path, version: &str) -> String {
        req_ok(
            topics::GAWD_SOLVE,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
                "version": version
            }),
        )
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
    }

    pub fn sanitize_input(input: &str) -> Result<String, String> {
        let v = req(topics::GAWD_SANITIZE, json!({ "input": input }))?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or(input)
            .to_string())
    }

    pub fn audit_action(tool: &str, detail: &str, workspace: &Path) -> Result<(), String> {
        let v = req(
            topics::GAWD_AUDIT_ACTION,
            json!({
                "tool": tool,
                "detail": detail,
                "workspace": workspace.display().to_string()
            }),
        )?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(())
    }

    pub fn apply_patch(payload: Value) -> Result<Value, String> {
        req(topics::GAWD_PATCH, payload)
    }

    /// Verified cluster roster as `(node_id, address)` pairs — used by
    /// commit-ledger anti-entropy to resolve a coordinator's address.
    pub fn cluster_peers() -> Result<Vec<(String, String)>, String> {
        let v = req(topics::GAWD_CLUSTER_PEERS, json!({}))?;
        let peers = v
            .get("peers")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let id = p.get("node_id")?.as_str()?.to_string();
                        let addr = p.get("address")?.as_str()?.to_string();
                        Some((id, addr))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(peers)
    }

    pub fn bloat_audit(workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_BLOAT,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn identity_report(workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_IDENTITY,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn audit_reasoning_substrate(workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_REASON_AUDIT,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn self_validate(workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_SELF_VALIDATE,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn train_reflexes(workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_TRAIN_REFLEX,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn resolve_capability_gap(name: &str, workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_CAPABILITY_GAP,
            json!({
                "name": name,
                "workspace": workspace.display().to_string()
            }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn broadcast_lock_request(resource_id: &str) -> bool {
        req_ok(
            topics::GAWD_LOCK_BROADCAST,
            json!({ "resource_id": resource_id }),
        )
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    }

    pub fn scheduler_recent_decisions(limit: usize) -> Value {
        req_ok(topics::GAWD_SCHEDULER_RECENT, json!({ "limit": limit }))
    }
}

// ── Tools facade ──────────────────────────────────────────────────────────

pub mod tools {
    use super::*;
    use crate::susi_error::{EaiError, EaiResult};

    pub fn exists(name: &str) -> bool {
        req_ok(topics::TOOLS_EXISTS, json!({ "name": name }))
            .get("exists")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn execute_tool(name: &str, args: &Value, workspace: &Path) -> EaiResult<String> {
        let v = req(
            topics::TOOLS_EXECUTE,
            json!({
                "name": name,
                "args": args,
                "workspace": workspace.display().to_string()
            }),
        )
        .map_err(EaiError::protocol)?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(EaiError::protocol(e));
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn auto_link_essential_mcp_servers() {
        let _ = req(topics::TOOLS_AUTO_LINK, json!({}));
    }

    pub fn scout_reasoning_remotes() -> Value {
        req_ok(topics::TOOLS_SCOUT_REMOTES, json!({}))
    }

    pub fn list_tools() -> Value {
        req_ok(topics::TOOLS_LIST, json!({}))
    }

    pub fn leading_mcp_list(workspace: &Path) -> Value {
        req_ok(
            topics::TOOLS_LEADING_LIST,
            json!({ "workspace": workspace.display().to_string() }),
        )
    }

    pub fn leading_mcp_get(workspace: &Path, name: &str) -> Value {
        req_ok(
            topics::TOOLS_LEADING_GET,
            json!({
                "workspace": workspace.display().to_string(),
                "name": name
            }),
        )
    }

    pub fn leading_mcp_remove(workspace: &Path, name: &str) -> Value {
        req_ok(
            topics::TOOLS_LEADING_REMOVE,
            json!({
                "workspace": workspace.display().to_string(),
                "name": name
            }),
        )
    }

    pub fn execute_external_tool(remote: &str, tool: &str, goal: &str) -> EaiResult<String> {
        let v = req(
            topics::TOOLS_REMOTE_EXECUTE,
            json!({
                "remote": remote,
                "tool": tool,
                "goal": goal
            }),
        )
        .map_err(EaiError::protocol)?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(EaiError::protocol(e));
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }
}

pub mod agents {
    use super::*;
    use crate::agent_types::AgentProfile;

    /// Plane-bus facade for agent metadata (backed by `susi-agents` handler).
    pub struct AgentMetaRegistry;

    impl AgentMetaRegistry {
        pub fn global() -> &'static Self {
            static REG: AgentMetaRegistry = AgentMetaRegistry;
            &REG
        }

        pub fn list_agents(&self) -> Vec<AgentProfile> {
            meta_list()
        }

        pub fn register_agent(&self, profile: AgentProfile) {
            meta_register(profile);
        }

        pub fn update_rank(&self, name: &str, delta: f32, source: &str) {
            meta_update_rank(name, delta, source);
        }

        pub fn get_checksum(&self) -> u64 {
            let agents = self.list_agents();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            for agent in agents.iter() {
                agent.name.hash(&mut hasher);
                agent.description.hash(&mut hasher);
            }
            hasher.finish()
        }
    }

    pub fn meta_list() -> Vec<AgentProfile> {
        let v = req_ok(topics::AGENTS_META_LIST, json!({}));
        serde_json::from_value(v).unwrap_or_default()
    }

    pub fn meta_register(profile: AgentProfile) {
        let _ = req(topics::AGENTS_META_REGISTER, json!({ "profile": profile }));
    }

    pub fn meta_update_rank(name: &str, delta: f32, source: &str) {
        let _ = req(
            topics::AGENTS_META_UPDATE_RANK,
            json!({ "name": name, "delta": delta, "source": source }),
        );
    }

    pub fn external_managed_goal(
        name: &str,
        goal: &str,
        workspace: &Path,
    ) -> Result<String, String> {
        let v = req(
            topics::AGENTS_EXTERNAL_MANAGED,
            json!({
                "name": name,
                "goal": goal,
                "workspace": workspace.display().to_string()
            }),
        )?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn external_catalog(workspace: &Path, kind: &str) -> Value {
        req_ok(
            topics::AGENTS_EXTERNAL_CATALOG,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind
            }),
        )
    }

    pub fn external_list(workspace: &Path, kind: &str) -> Value {
        req_ok(
            topics::AGENTS_EXTERNAL_LIST,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind
            }),
        )
    }

    pub fn external_run(
        workspace: &Path,
        kind: &str,
        agent: &str,
        prompt: &str,
    ) -> Result<Value, String> {
        req(
            topics::AGENTS_EXTERNAL_RUN,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind,
                "agent": agent,
                "prompt": prompt
            }),
        )
    }

    pub fn external_logs(workspace: &Path, kind: &str, run_id: &str) -> Result<String, String> {
        let v = req(
            topics::AGENTS_EXTERNAL_LOGS,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind,
                "run_id": run_id
            }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub fn external_runs(workspace: &Path, kind: &str) -> Value {
        req_ok(
            topics::AGENTS_EXTERNAL_RUNS,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind
            }),
        )
    }

    pub fn external_control(
        workspace: &Path,
        kind: &str,
        task_id: &str,
        action: &str,
        arg: &Value,
    ) -> Result<Value, String> {
        req(
            topics::AGENTS_EXTERNAL_CONTROL,
            json!({
                "workspace": workspace.display().to_string(),
                "kind": kind,
                "task_id": task_id,
                "action": action,
                "stderr": arg.get("stderr"),
                "message": arg.get("message"),
                "refresh": arg.get("refresh"),
            }),
        )
    }
}

pub mod gawd_hooks {
    use super::*;
    use crate::susi_error::{EaiError, EaiResult};

    pub fn audit_action(tool: &str, detail: &str, workspace: &Path) -> EaiResult<()> {
        gawd::audit_action(tool, detail, workspace).map_err(EaiError::governance)
    }

    pub fn sanitize_input(input: &str) -> EaiResult<String> {
        gawd::sanitize_input(input).map_err(EaiError::governance)
    }

    pub fn apply_patch_cycle(
        workspace: &Path,
        request_json: &str,
        trust_level: &str,
    ) -> EaiResult<String> {
        let v = gawd::apply_patch(json!({
            "workspace": workspace.display().to_string(),
            "request_json": request_json,
            "trust_level": trust_level
        }))
        .map_err(EaiError::governance)?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(EaiError::governance(e));
        }
        Ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Echo;

    impl PlaneHandler for Echo {
        fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
            Ok(json!({ "topic": topic, "payload": payload }))
        }
    }

    #[test]
    fn request_round_trip() {
        // Isolate from other tests mutating the global bus.
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let bus = PlaneBus::global();
        bus.register("test.echo", Arc::new(Echo));
        let v = bus
            .request("test.echo", json!({ "x": 1 }))
            .expect("handler");
        assert_eq!(v["topic"], "test.echo");
        assert_eq!(v["payload"]["x"], 1);
    }
}
