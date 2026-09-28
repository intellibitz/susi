//! In-process **plane message bus** — the only allowed control plane between
//! feature crates (gawd / gemi / gmcp / tools / agents / server).
//!
//! Feature planes must not depend on each other in Cargo.toml. They talk only
//! through this bus (and shared foundation: paths / error / core / config /
//! sandbox / native). Composition roots (`susi-daemon`, CLI) register handlers
//! and may still link every plane.

use crate::susi_core::plane_bus_ipc::IpcPlaneBus;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Rebuild a handler's displayed error (`"Protocol Error: ..."`) as the
/// same `EaiError` kind instead of wrapping it in another layer, which
/// rendered "Protocol Error: Protocol Error: ..." and lost the real kind.
fn remote_error(display: &str) -> crate::susi_error::EaiError {
    const KINDS: [(&str, &str); 13] = [
        ("Governance Violation: ", "Governance"),
        ("Hardware Error: ", "Hardware"),
        ("Protocol Error: ", "Protocol"),
        ("Inference Error: ", "Inference"),
        ("Sandbox Error: ", "Sandbox"),
        ("Configuration Error: ", "Config"),
        ("I/O Error: ", "Io"),
        ("Network Error: ", "Network"),
        ("Filesystem Error: ", "Filesystem"),
        ("Process Error: ", "Process"),
        ("Authentication Error: ", "Authentication"),
        ("Authorization Error: ", "Authorization"),
        ("Internal Engine Error: ", "Internal"),
    ];
    KINDS
        .iter()
        .find(|(prefix, _)| display.starts_with(prefix))
        .map_or_else(
            || crate::susi_error::EaiError::protocol(display),
            |(_, kind)| crate::susi_error::rewrap(kind, display.to_string()),
        )
}

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
    /// Embedding vector for a text — the handler tries registered
    /// providers in order and returns the first successful vector.
    pub const GEMI_INFER_EMBED: &str = "gemi.infer.embed";
    pub const GEMI_PLAN_PARTITION: &str = "gemi.plan.partition";
    pub const GEMI_PLAN_MISSION: &str = "gemi.plan.mission";
    pub const GEMI_PLAN_REFINE: &str = "gemi.plan.refine";
    pub const GEMI_INTENT_CLASSIFY: &str = "gemi.intent.classify";
    pub const GEMI_ALPHA_PROJECTION: &str = "gemi.alpha.projection";
    pub const GEMI_ALPHA_TRAIN: &str = "gemi.alpha.train";
    pub const GEMI_TELEMETRY_SAMPLE: &str = "gemi.telemetry.sample";
    pub const GEMI_MODELS_LOADED: &str = "gemi.models.loaded";
    pub const GEMI_MODELS_PRELOAD: &str = "gemi.models.preload";
    pub const GEMI_MODELS_UNLOAD: &str = "gemi.models.unload";
    pub const GEMI_CLOUD_REGISTER: &str = "gemi.cloud.register";
    pub const GEMI_CLOUD_FAILOVER: &str = "gemi.cloud.failover";
    /// Explain the live local/cloud inference placement without executing it.
    pub const GEMI_ROUTING_PLAN: &str = "gemi.routing.plan";
    /// Clear a repaired provider's routing quarantine.
    pub const GEMI_ROUTING_CLEAR_COOLDOWN: &str = "gemi.routing.clear_cooldown";
    /// Report a failed provider attempt so the GEMI plane's cooldown
    /// routing can skip it (and its credential/endpoint scope) — used by
    /// consumers that call providers directly, e.g. swarm recovery.
    pub const GEMI_PROVIDER_FAILURE: &str = "gemi.provider.failure";
    /// Report a provider success so a stale cooldown clears early —
    /// counterpart to `GEMI_PROVIDER_FAILURE`.
    pub const GEMI_PROVIDER_SUCCESS: &str = "gemi.provider.success";
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
    pub const TOOLS_LEADING_ENABLE: &str = "tools.leading.enable";
    pub const TOOLS_LEADING_DISABLE: &str = "tools.leading.disable";

    pub const AGENTS_EXTERNAL_MANAGED: &str = "agents.external.managed";
    pub const AGENTS_EXTERNAL_CATALOG: &str = "agents.external.catalog";
    pub const AGENTS_EXTERNAL_LOGS: &str = "agents.external.logs";
    pub const AGENTS_EXTERNAL_RUNS: &str = "agents.external.runs";
    pub const AGENTS_EXTERNAL_LIST: &str = "agents.external.list";
    pub const AGENTS_EXTERNAL_CONTROL: &str = "agents.external.control";
    pub const GEMI_MODELS_SELECT_MIN: &str = "gemi.models.select_min";
    pub const GEMI_CODING_PREFER: &str = "gemi.coding.prefer";

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
/// this `global()` are visible to every plane in the process and, over the
/// shared `<cache>/bus/<pid>/` rendezvous, to sibling cell processes.
pub struct PlaneBus {
    inner: Arc<IpcPlaneBus>,
}

impl PlaneBus {
    pub fn global() -> &'static PlaneBus {
        static BUS: OnceLock<PlaneBus> = OnceLock::new();
        BUS.get_or_init(|| PlaneBus {
            // Shared with registry_ipc/broker/etc. so one listener serves
            // all modules.
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

    /// Bounded provider identifier accepted across CLI, HTTP, MCP, and the
    /// plane bus. This is a wire invariant and deliberately performs no I/O.
    pub fn valid_provider_id(value: &str) -> bool {
        !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
    }

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

        /// Return the GEMI plane's live, explainable local/cloud placement.
        /// The value stays schema-flexible at the kernel boundary while the
        /// owning plane retains the strongly typed policy implementation.
        pub fn placement(requires: Option<&str>, max_cost: Option<f64>) -> Value {
            Self::placement_with_cloud(requires, max_cost, true)
        }

        /// Placement with a request-scoped cloud egress decision. This keeps
        /// privacy intent on the kernel message instead of relying on mutable
        /// process-wide routing configuration.
        pub fn placement_with_cloud(
            requires: Option<&str>,
            max_cost: Option<f64>,
            allow_cloud: bool,
        ) -> Value {
            req_ok(
                topics::GEMI_ROUTING_PLAN,
                json!({
                    "requires": requires,
                    "max_cost": max_cost,
                    "allow_cloud": allow_cloud,
                }),
            )
        }

        /// Re-admit a repaired provider to placement. Returns `true` only
        /// when a provider or vendor-scope quarantine was actually removed.
        pub fn clear_provider_cooldown(provider: &str) -> Result<bool, String> {
            let value = req(
                topics::GEMI_ROUTING_CLEAR_COOLDOWN,
                json!({ "provider": provider }),
            )?;
            value
                .get("cleared")
                .and_then(Value::as_bool)
                .ok_or_else(|| "GEMI cooldown reset returned no boolean result".to_string())
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
            Self::generate_reasoning_stream_with_model(prompt, workspace, on_chunk, None)
        }

        /// Streaming reasoning with an optional caller-requested model —
        /// the GEMI handler threads it into provider/model selection.
        pub fn generate_reasoning_stream_with_model(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
        ) -> String {
            Self::generate_reasoning_stream_with_model_meta(
                prompt,
                workspace,
                on_chunk,
                model,
                &|_| {},
            )
        }

        /// Streaming reasoning that also surfaces `susi_meta` control
        /// chunks — the handler emits `{"susi_meta":{"provider":name}}`
        /// when routing picks the actual serving backend, before any
        /// content chunk, so SSE callers can label frames truthfully.
        pub fn generate_reasoning_stream_with_model_meta(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
            on_meta: &dyn Fn(&str),
        ) -> String {
            let bus = PlaneBus::global();
            let (stream_id, rx) = bus.open_stream();
            let result = req(
                topics::GEMI_INFER_STREAM,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string(),
                    "stream_id": stream_id,
                    "model": model,
                }),
            );
            let deliver = |chunk: &Value| {
                if let Some(s) = chunk.as_str() {
                    on_chunk(s.to_string());
                } else if let Some(s) = chunk.get("text").and_then(|v| v.as_str()) {
                    on_chunk(s.to_string());
                } else if let Some(p) = chunk
                    .get("susi_meta")
                    .and_then(|m| m.get("provider"))
                    .and_then(|v| v.as_str())
                {
                    on_meta(p);
                }
            };
            while let Ok(chunk) = rx.try_recv() {
                deliver(&chunk);
            }
            // Drain remaining after handler returns
            while let Ok(chunk) = rx.recv_timeout(std::time::Duration::from_millis(10)) {
                deliver(&chunk);
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

        /// Embed `text` via the first registered provider that can —
        /// backs the OpenAI-compatible `/v1/embeddings` surface. Returns
        /// the provider's raw vector; `None` when no provider can embed.
        pub fn embed(text: &str, model: Option<&str>) -> Option<Vec<f32>> {
            let v = req_ok(
                topics::GEMI_INFER_EMBED,
                json!({ "text": text, "model": model }),
            );
            v.get("embedding").and_then(|e| e.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect()
            })
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

    /// Weights resident in the GEMI plane's inference cache (see
    /// `InferenceHost::loaded_models`).
    pub fn loaded_models() -> Value {
        req_ok(topics::GEMI_MODELS_LOADED, json!({}))
    }

    /// Load `model` into the GEMI plane's inference cache.
    pub fn preload_model(model: &str) -> Result<Value, String> {
        req(topics::GEMI_MODELS_PRELOAD, json!({ "model": model }))
    }

    /// Evict `model` from the GEMI plane's inference cache.
    pub fn unload_model(model: &str) -> Result<Value, String> {
        req(topics::GEMI_MODELS_UNLOAD, json!({ "model": model }))
    }

    pub fn register_configured_cloud_endpoints() -> Value {
        req_ok(topics::GEMI_CLOUD_REGISTER, json!({}))
    }

    pub fn cloud_failover_order() -> Value {
        req_ok(topics::GEMI_CLOUD_FAILOVER, json!({}))
    }

    /// Report a provider attempt failure to the GEMI plane's cooldown
    /// router — fire-and-forget; cooldown state is advisory.
    pub fn note_provider_failure(name: &str, error: &str) {
        let _ = req(
            topics::GEMI_PROVIDER_FAILURE,
            json!({ "name": name, "error": error }),
        );
    }

    /// Report a provider success — clears any stale cooldown on the GEMI
    /// side. Fire-and-forget.
    pub fn note_provider_success(name: &str) {
        let _ = req(topics::GEMI_PROVIDER_SUCCESS, json!({ "name": name }));
    }

    pub fn coding_catalog() -> Value {
        req_ok(topics::GEMI_CODING_CATALOG, json!({}))
    }

    /// Prefer a coding/agent model on the live GEMI plane (preflight,
    /// persisted preference, runtime model override) — the same operation
    /// as `susi model prefer`.
    pub fn coding_prefer(id: &str) -> Result<Value, String> {
        req(topics::GEMI_CODING_PREFER, json!({ "id": id }))
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
        solve_mission_with_model(intent, workspace, version, None)
    }

    /// Mission solve with an optional caller-requested model hint —
    /// `/v1/chat/completions` threads its `model` field here so failover
    /// prioritizes the named provider/model instead of silently rerouting.
    pub fn solve_mission_with_model(
        intent: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
    ) -> String {
        req_ok(
            topics::GAWD_SOLVE,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
                "version": version,
                "model": model
            }),
        )
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
    }

    /// Generative mission solve (`/v1/chat/completions`): the model's own
    /// output is the product, so recovery may self-cite an inference
    /// receipt minted for the provider call instead of demanding
    /// mission-captured tool evidence a chat goal never produces.
    pub fn solve_mission_generative(
        intent: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
    ) -> String {
        req_ok(
            topics::GAWD_SOLVE,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
                "version": version,
                "model": model,
                "generative": true
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
        Ok(cluster_roster()?
            .into_iter()
            .map(|(id, addr, _)| (id, addr))
            .collect())
    }

    /// Verified roster with trust scores, sorted most-trusted first —
    /// anti-entropy repair tries candidates in this order so the most
    /// reliable replica is asked before fallbacks.
    pub fn cluster_roster() -> Result<Vec<(String, String, f64)>, String> {
        let v = req(topics::GAWD_CLUSTER_PEERS, json!({}))?;
        let mut peers: Vec<(String, String, f64)> = v
            .get("peers")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let id = p.get("node_id")?.as_str()?.to_string();
                        let addr = p.get("address")?.as_str()?.to_string();
                        let trust = p.get("trust_score").and_then(|t| t.as_f64()).unwrap_or(0.0);
                        Some((id, addr, trust))
                    })
                    .collect()
            })
            .unwrap_or_default();
        peers.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
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
            return Err(super::remote_error(e));
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

    /// Enable a leading MCP server into the host MCP config; returns the
    /// written server config. Errors are errors, not `{"error": …}` values.
    pub fn leading_mcp_enable(workspace: &Path, name: &str) -> Result<Value, String> {
        req(
            topics::TOOLS_LEADING_ENABLE,
            json!({
                "workspace": workspace.display().to_string(),
                "name": name
            }),
        )
    }

    /// Disable a leading MCP server; `{"removed": bool}` on success.
    pub fn leading_mcp_disable(workspace: &Path, name: &str) -> Result<Value, String> {
        req(
            topics::TOOLS_LEADING_DISABLE,
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
            return Err(super::remote_error(e));
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }
}

pub mod agents {
    use super::*;
    use crate::susi_core::agent_types::AgentProfile;

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
            return Err(super::remote_error(e));
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

    #[test]
    fn provider_id_wire_contract_is_bounded_and_printable() {
        assert!(gemi::valid_provider_id("openai-gpt-4o-mini"));
        assert!(!gemi::valid_provider_id(""));
        assert!(!gemi::valid_provider_id("bad id"));
        assert!(!gemi::valid_provider_id(&"x".repeat(257)));
    }
}

#[cfg(test)]
mod remote_error_tests {
    use super::remote_error;

    #[test]
    fn remote_errors_keep_their_kind_without_double_prefix() {
        let e = remote_error("Protocol Error: task_id is required");
        assert_eq!(e.kind_name(), "Protocol");
        assert_eq!(e.to_string(), "Protocol Error: task_id is required");
        let e = remote_error("Governance Violation: denied");
        assert_eq!(e.kind_name(), "Governance");
        assert_eq!(e.to_string(), "Governance Violation: denied");
        let e = remote_error("Sandbox Error: escape");
        assert_eq!(e.kind_name(), "Sandbox");
        let e = remote_error("no handler for topic");
        assert_eq!(e.to_string(), "Protocol Error: no handler for topic");
    }
}

#[cfg(test)]
mod facade_marshal_tests {
    //! The facade modules (`gemi::`, `gawd::`, `agents::`, `tools::`,
    //! `gawd_hooks::`) are the microkernel's typed seam over bus topics —
    //! every call marshals args, hits a topic, decodes the reply. A catch-all
    //! per prefix exercises marshal + decode deterministically in-process.
    use super::*;
    use std::sync::{Mutex, Once};

    static LOCK: Mutex<()> = Mutex::new(());
    static ONCE: Once = Once::new();

    struct Echo;
    impl PlaneHandler for Echo {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!({ "ok": true }))
        }
    }
    struct Embed;
    impl PlaneHandler for Embed {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!({ "embedding": [1.0, 2.0, 3.0] }))
        }
    }
    struct EmptyList;
    impl PlaneHandler for EmptyList {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Ok(json!([]))
        }
    }

    fn wire() -> std::sync::MutexGuard<'static, ()> {
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        ONCE.call_once(|| {
            let bus = PlaneBus::global();
            bus.register_prefix("gemi.", Arc::new(Echo));
            bus.register_prefix("gawd.", Arc::new(Echo));
            bus.register_prefix("agents.", Arc::new(EmptyList));
            bus.register_prefix("tools.", Arc::new(Echo));
            bus.register(topics::GEMI_INFER_EMBED, Arc::new(Embed));
        });
        g
    }

    #[test]
    fn bus_stream_channel_round_trips() {
        let _g = wire();
        let bus = PlaneBus::global();
        let (id, rx) = bus.open_stream();
        bus.stream_emit(&id, json!({ "chunk": 1 }));
        bus.stream_emit(&id, json!({ "chunk": 2 }));
        bus.stream_close(&id);
        let got: Vec<Value> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["chunk"], 1);
        // Emitting after close is a no-op, not a panic.
        bus.stream_emit(&id, json!({ "chunk": 3 }));
        assert!(rx.try_iter().count() == 0);
        // Unknown topic request errors rather than hanging.
        assert!(
            bus.request("no.such.topic.zzz", json!({})).is_err()
                || bus.is_wired("no.such.topic.zzz")
        );
    }

    #[test]
    fn gemi_facade_calls_marshal_through_the_bus() {
        let _g = wire();
        let ws = Path::new(".");
        use gemi::*;
        let _ = HardwareProfiler::get_profile();
        let _ = HardwareProfiler::check_oom_critical();
        let _ = HardwareProfiler::get_caps_string();
        let _ = HardwareProfiler::get_candle_device_label();
        let _ = ModelManager::list_models(ws);
        let _ = ModelManager::get_selected_model(Some(ws));
        let _ = ModelManager::get_selected_model(None);
        let _ = ModelManager::get_active_engine_and_model(Some("code"));
        let _ = ModelManager::get_model_path("m");
        let _ = ModelManager::verify_local_models(ws);
        let _ = ModelManager::install_model("id");
        let _ = ModelManager::ensure_hardware_optimal_models(ws);
        let _ = ModelManager::deep_scan_home_and_register(ws);
        let _ = ModelManager::set_selected_model("m");
        let _ = ModelManager::get_selected_model_for_request_with_min_complexity(
            "p",
            Some(ws),
            Some("low"),
        );
        let _ = ModelManager::list_models_len(ws);
        let _ = ModelManager::placement(Some("gpu"), Some(0.5));
        let _ = ModelManager::placement_with_cloud(Some("p"), Some(0.5), true);
        let _ = ModelManager::clear_provider_cooldown("openrouter");
        let _ = loaded_models();
        assert!(preload_model("m").is_ok());
        assert!(unload_model("m").is_ok());
        let _ = register_configured_cloud_endpoints();
        let _ = cloud_failover_order();
        note_provider_failure("p", "e");
        note_provider_success("p");
        let _ = coding_catalog();
        assert!(coding_prefer("id").is_ok());
        let _ = pulse_reason("why", ws);
        let _ = IntentClassifier::classify("goal");
        assert_eq!(
            GemiEngine::embed("t", None).as_deref(),
            Some(&[1.0f32, 2.0, 3.0][..])
        );
        let _ = GemiEngine::embed("t", Some("m"));
        let _ = GemiEngine::verify_axiomatic_alignment("t", ws);
        let _ = GemiEngine::generate_reasoning("p", ws);
        let _ = GemiEngine::generate_reasoning_deep("p", ws);
        let _ = GemiEngine::generate_reasoning_deep_with_min_complexity("p", ws, "low");
        let _ = MissionPlanner::partition_mission("g", ws);
        let _ = MissionPlanner::plan_mission("g", ws);
        let _ = MissionPlanner::refine_plan("g", &json!({}), ws);
    }

    #[test]
    fn gawd_facade_calls_marshal_through_the_bus() {
        let _g = wire();
        let ws = Path::new(".");
        let _ = gawd::solve_mission("i", ws, "v");
        let _ = gawd::solve_mission_with_model("i", ws, "v", Some("m"));
        let _ = gawd::solve_mission_generative("i", ws, "v", None);
        assert!(gawd::sanitize_input("x").is_ok());
        assert!(gawd::audit_action("t", "d", ws).is_ok());
        let _ = gawd::apply_patch(json!({}));
        let _ = gawd::cluster_peers();
        let _ = gawd::cluster_roster();
        let _ = gawd::bloat_audit(ws);
        let _ = gawd::identity_report(ws);
        let _ = gawd::audit_reasoning_substrate(ws);
        let _ = gawd::self_validate(ws);
        let _ = gawd::train_reflexes(ws);
        let _ = gawd::resolve_capability_gap("cap", ws);
        let _ = gawd::broadcast_lock_request("res");
        let _ = gawd::scheduler_recent_decisions(5);
    }

    #[test]
    fn tools_and_agents_facades_marshal_through_the_bus() {
        let _g = wire();
        let ws = Path::new(".");
        let _ = tools::exists("exec_command");
        let _ = tools::execute_tool("exec_command", &json!({}), ws);
        tools::auto_link_essential_mcp_servers();
        let _ = tools::scout_reasoning_remotes();
        let _ = tools::list_tools();
        let _ = tools::leading_mcp_list(ws);
        let _ = tools::leading_mcp_enable(ws, "n");
        let _ = tools::leading_mcp_disable(ws, "n");
        let _ = tools::execute_external_tool("r", "t", "g");

        let profile = crate::agent_types::AgentProfile {
            name: "n".into(),
            description: "d".into(),
            categories: vec![],
            semantic_anchors: vec![],
            base_rank: 0.5,
            is_core: false,
        };
        let _ = agents::meta_list();
        agents::meta_register(profile);
        agents::meta_update_rank("n", 0.1, "test");
        let reg = agents::AgentMetaRegistry::global();
        let _ = reg.get_checksum();
        let _ = agents::external_catalog(ws, "execution");
        let _ = agents::external_list(ws, "execution");
        let _ = agents::external_runs(ws, "execution");
        let _ = agents::external_run(ws, "execution", "a", "p");
        let _ = agents::external_logs(ws, "execution", "id");
        let _ = agents::external_control(ws, "execution", "id", "status", &json!({}));
        let _ = agents::external_managed_goal("n", "g", ws);
    }

    #[test]
    fn gawd_hooks_facades_marshal() {
        let _g = wire();
        let ws = Path::new(".");
        let _ = gawd_hooks::audit_action("t", "d", ws);
        let _ = gawd_hooks::sanitize_input("x");
    }
}
