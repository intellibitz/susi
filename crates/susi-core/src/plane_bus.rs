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
    /// `susi top --json`'s unified operator state for a workspace.
    pub const GEMI_OPERATOR_SNAPSHOT: &str = "gemi.operator.snapshot";
    /// `susi ecosystem start <id>`: start an installed local AI engine.
    pub const GEMI_ECOSYSTEM_START: &str = "gemi.ecosystem.start";
    /// `susi ecosystem kb <list|show|search|check>`: the cloud-AI-ecosystem
    /// knowledge base.
    pub const GEMI_ECOSYSTEM_KB: &str = "gemi.ecosystem.kb";
    /// `susi ecosystem explain <id>`: one knowledge-base entity end to end.
    pub const GEMI_ECOSYSTEM_EXPLAIN: &str = "gemi.ecosystem.explain";

    pub const GAWD_SOLVE: &str = "gawd.mission.solve";
    /// The CLI's own mission (`susi <goal>`) run by the daemon: on-demand
    /// engine start allowed, answered with `{text, status, exit_code}`.
    pub const GAWD_MISSION_RUN: &str = "gawd.mission.run";
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
    /// `susi plan`: the autonomous planning loop for one goal.
    pub const GAWD_PLAN_AUTONOMOUS: &str = "gawd.mission.plan";
    /// `susi pulse` / `susi admin pulse`: ingest a natural-language intent
    /// into the workspace evidence ledger.
    pub const GAWD_PULSE_INGEST: &str = "gawd.admin.pulse";
    /// `susi audit` / `susi admin audit`: the deterministic compliance audit.
    pub const GAWD_AUDIT_COMPLIANCE: &str = "gawd.admin.audit_compliance";
    /// `susi admin audit-evidence export`: wrap redacted audit segments in
    /// the independently verifiable export format.
    pub const GAWD_AUDIT_EVIDENCE_EXPORT: &str = "gawd.admin.audit_evidence_export";
    /// `susi substrate-report`: the compiled-constants substrate report.
    pub const GAWD_SUBSTRATE_REPORT: &str = "gawd.admin.substrate_report";
    /// Verified cluster roster (node_id ↔ address) for commit-ledger
    /// anti-entropy: a receiver that detects a seq gap resolves the
    /// coordinator's address here, then pulls the missing records.
    pub const GAWD_CLUSTER_PEERS: &str = "gawd.cluster.peers";

    pub const TOOLS_EXISTS: &str = "tools.registry.exists";
    pub const TOOLS_EXECUTE: &str = "tools.registry.execute";
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

    // Host composition root (`susi-daemon`): capabilities that need the
    // daemon crate's view of every plane (zero-UI plan B4 groups 4, 5, 8, 9).
    /// `susi privacy mode <mode>`: persist the mode and align routing.
    pub const DAEMON_PRIVACY_MODE: &str = "daemon.privacy.mode";
    /// `susi ambient start`: the background ambient indexer for a workspace.
    pub const DAEMON_AMBIENT_START: &str = "daemon.ambient.start";
    /// `susi context-graph sample-os`: one OS context sample into the graph.
    pub const DAEMON_CONTEXT_SAMPLE_OS: &str = "daemon.context.sample_os";
    /// `susi ecosystem scan|status`: the local AI ecosystem scan (fresh or
    /// the last persisted one).
    pub const DAEMON_ECOSYSTEM_SCAN: &str = "daemon.ecosystem.scan";
    /// `susi substrate status`: Pluggable / Sandbox / Governance /
    /// Host-contract / Reflexes status.
    pub const DAEMON_SUBSTRATE_STATUS: &str = "daemon.substrate.status";
    /// `susi crown verify|status`: the Tier S USP checks.
    pub const DAEMON_CROWN_VERIFY: &str = "daemon.crown.verify";
    /// `susi os provision <probe|list|apply>`: explicit cloud provisioning.
    pub const DAEMON_OS_PROVISION: &str = "daemon.os.provision";
    /// `susi services <start|stop|restart>`: operator control of a
    /// supervised leaf service.
    pub const DAEMON_SERVICES_CONTROL: &str = "daemon.services.control";
    /// `susi openweight|frontier|openrouter`, `susi keys` (presence only),
    /// `susi brain`, `susi benchmark` and `susi verify-download-agent`
    /// (zero-UI plan B4 group 3): `{action, ..}`.
    pub const DAEMON_PROVIDERS: &str = "daemon.providers";
    /// `susi agents|frameworks|models|mcp doctor|setup|configure|reset`,
    /// `retry` and `susi mcp-add` (zero-UI plan B4 group 7): `{action, ..}`.
    pub const DAEMON_ADAPTERS: &str = "daemon.adapters";
    /// `susi harness plan|author|run|status|budget|models` (zero-UI plan B4
    /// group 1): `{action, ..}`.
    pub const DAEMON_HARNESS: &str = "daemon.harness";
    /// Host maintenance and views with no other plane: `susi status`,
    /// `review`, `os status|runtime|ecosystem|manage`, `peers
    /// add|remove|unban|rekey`, `auto`, `deep-scan`, `extensions create`,
    /// `admin sync|verify|reload|provider-contract|intent-ladder` and the
    /// daemon lifecycle (`stop`, `restart`): `{action, ..}`.
    pub const DAEMON_HOST: &str = "daemon.host";
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
    pub fn open_stream(&self) -> (String, std::sync::mpsc::Receiver<Value>) {
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

/// Payload key naming the bus stream a watched request's progress lines
/// travel back on.
const PROGRESS_STREAM: &str = "progress_stream";

/// Stream chunk key naming the handler's reverse cancellation stream.
const CANCEL_STREAM: &str = "cancel_stream";

/// [`req`] for a call this thread's caller may be watching or may cancel
/// ([`crate::progress`]). With a sink or cancel flag installed, the handler
/// gets a bus stream ([`with_watched`], [`progress_sink_for`]) so progress
/// lines come back and cancellation travels forward while the request runs;
/// without either this is [`req`].
fn req_watched(topic: &str, mut payload: Value) -> Result<Value, String> {
    let sink = crate::progress::current();
    let cancel = crate::progress::current_cancel();
    if sink.is_none() && cancel.is_none() {
        return req(topic, payload);
    }
    let (stream_id, rx) = PlaneBus::global().open_stream();
    payload[PROGRESS_STREAM] = json!(stream_id);
    if cancel
        .as_ref()
        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
    {
        payload["cancelled"] = json!(true);
    }
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = done.clone();
    // Relay while the request blocks this thread; once it has returned,
    // drain what is already queued and stop at the first quiet gap.
    let relay = std::thread::spawn(move || {
        let mut cancel_streams = Vec::<String>::new();
        let mut cancel_sent = false;
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(25)) {
                Ok(chunk) => {
                    if let (Some(sink), Some(line)) =
                        (&sink, chunk.get("message").and_then(Value::as_str))
                    {
                        sink(line);
                    }
                    if let Some(cs) = chunk.get(CANCEL_STREAM).and_then(Value::as_str) {
                        if cancel_sent
                            || cancel
                                .as_ref()
                                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
                        {
                            cancel_sent = true;
                            PlaneBus::global().stream_emit(cs, json!({ "cancelled": true }));
                        } else {
                            cancel_streams.push(cs.to_string());
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if finished.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !cancel_sent
                && cancel
                    .as_ref()
                    .is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
            {
                cancel_sent = true;
                for cs in cancel_streams.drain(..) {
                    PlaneBus::global().stream_emit(&cs, json!({ "cancelled": true }));
                }
            }
        }
    });
    // Ends the relay and closes the stream on every way out, a panic too.
    struct Watch {
        done: Arc<std::sync::atomic::AtomicBool>,
        relay: Option<std::thread::JoinHandle<()>>,
        stream_id: String,
    }
    impl Drop for Watch {
        fn drop(&mut self) {
            self.done.store(true, std::sync::atomic::Ordering::Release);
            if let Some(relay) = self.relay.take() {
                let _ = relay.join();
            }
            PlaneBus::global().stream_close(&self.stream_id);
        }
    }
    let _watch = Watch {
        done,
        relay: Some(relay),
        stream_id,
    };
    req(topic, payload)
}

/// The progress sink a plane handler installs ([`crate::progress::with_sink`])
/// for a watched request: each line goes back over the request's stream.
/// `None` when nobody watches the request.
#[must_use]
pub fn progress_sink_for(payload: &Value) -> Option<crate::progress::Sink> {
    let stream_id = payload
        .get(PROGRESS_STREAM)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?
        .to_string();
    Some(Arc::new(move |line: &str| {
        PlaneBus::global().stream_emit(&stream_id, json!({ "message": line }));
    }))
}

/// Run a plane handler body with both the watched request's progress sink and
/// its cooperative cancellation flag installed on this thread.
pub fn with_watched<T>(payload: &Value, call: impl FnOnce() -> T) -> T {
    let Some(stream_id) = payload
        .get(PROGRESS_STREAM)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        return call();
    };
    let initially_cancelled = payload
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(initially_cancelled));
    let (cancel_stream, cancel_rx) = PlaneBus::global().open_stream();
    let observed = cancel_flag.clone();
    let listener = std::thread::spawn(move || {
        if cancel_rx.recv().is_ok() {
            observed.store(true, std::sync::atomic::Ordering::Release);
        }
    });
    PlaneBus::global().stream_emit(&stream_id, json!({ CANCEL_STREAM: cancel_stream }));
    struct CancelWatch {
        cancel_stream: String,
        listener: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for CancelWatch {
        fn drop(&mut self) {
            PlaneBus::global().stream_close(&self.cancel_stream);
            if let Some(listener) = self.listener.take() {
                let _ = listener.join();
            }
        }
    }
    let _cancel_watch = CancelWatch {
        cancel_stream,
        listener: Some(listener),
    };
    let sink: crate::progress::Sink = Arc::new(move |line: &str| {
        PlaneBus::global().stream_emit(&stream_id, json!({ "message": line }));
    });
    crate::progress::with_sink(Some(sink), || {
        crate::progress::with_cancel(Some(cancel_flag), call)
    })
}

// ── Gemi facade (call sites use these instead of `susi_gemi::*`) ──────────

pub mod gemi {
    use super::*;

    /// Prefix of a GEMI reply that carries the tool calls a model made with
    /// tools the caller supplied: `{"content": .., "tool_calls": [{"name",
    /// "arguments"}]}` follows. The caller runs its own tools, so GEMI returns
    /// the calls instead of executing them; the 9091 server turns them into
    /// OpenAI `tool_calls`.
    pub const CALLER_TOOL_CALLS: &str = "[SUSI CALLER_TOOL_CALLS]";

    /// Prefix of a GEMI reply the model stopped because it reached the
    /// caller's `max_tokens`, so the 9091 server answers `"length"`.
    pub const LENGTH_CUT: &str = "[SUSI LENGTH_CUT]";

    /// Bounded provider identifier accepted across CLI, HTTP, MCP, and the
    /// plane bus. This is a wire invariant and deliberately performs no I/O.
    pub fn valid_provider_id(value: &str) -> bool {
        !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
    }

    /// A placement `requires` capability: one of the capabilities the
    /// router can satisfy, as a bounded token. Shared by every surface that
    /// accepts placement constraints (chat completions, the MCP
    /// `inference_placement` tool) so they refuse the same inputs.
    pub fn validated_requirement(value: &str) -> Result<String, &'static str> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("requires must be 1-64 letters, numbers, '-' or '_'");
        }
        if !matches!(
            value.to_ascii_lowercase().as_str(),
            "text" | "chat" | "reasoning" | "code" | "vision" | "tools"
        ) {
            return Err("requires must be one of: text, chat, reasoning, code, vision, tools");
        }
        Ok(value.to_string())
    }

    /// A placement `max_cost`: a finite, non-negative number. `None` (absent
    /// or not a number) is an error, since the caller supplied the field.
    pub fn validated_max_cost(value: Option<f64>) -> Result<f64, &'static str> {
        value
            .filter(|cost| cost.is_finite() && *cost >= 0.0)
            .ok_or("max_cost must be a non-negative finite number")
    }

    /// Request-scoped placement constraints, validated.
    #[derive(Debug, Clone, PartialEq)]
    pub struct PlacementConstraints {
        pub requires: Option<String>,
        pub max_cost: Option<f64>,
        /// Defaults to `true` when the caller leaves it out.
        pub allow_cloud: bool,
    }

    /// Read `requires`, `max_cost` and `allow_cloud` from a JSON object.
    /// An absent or `null` field takes its default; a present but malformed
    /// field is an error, never a default: a non-boolean `allow_cloud` must
    /// not quietly become `true`. `prefix` names the object in messages
    /// (`"susi."` for a completion's `susi` block, `""` for tool arguments).
    pub fn placement_constraints(
        object: Option<&serde_json::Map<String, Value>>,
        prefix: &str,
    ) -> Result<PlacementConstraints, String> {
        let field = |key: &str| object.and_then(|o| o.get(key)).filter(|v| !v.is_null());
        let requires = field("requires")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| format!("{prefix}requires must be a string"))
                    .and_then(|text| {
                        validated_requirement(text).map_err(|e| format!("{prefix}{e}"))
                    })
            })
            .transpose()?;
        let max_cost = field("max_cost")
            .map(|value| validated_max_cost(value.as_f64()).map_err(|e| format!("{prefix}{e}")))
            .transpose()?;
        let allow_cloud = match field("allow_cloud") {
            None => true,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| format!("{prefix}allow_cloud must be a boolean"))?,
        };
        Ok(PlacementConstraints {
            requires,
            max_cost,
            allow_cloud,
        })
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
                    "workspace": workspace.display().to_string(),
                    "on_demand_engine_start": crate::susi_core::provider::OnDemandEngineStart::allowed(),
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
                None,
            )
        }

        /// Streaming reasoning that also surfaces `susi_meta` control
        /// chunks — the handler emits `{"susi_meta":{"provider":name}}`
        /// when routing picks the actual serving backend, before any
        /// content chunk, so SSE callers can label frames truthfully.
        /// `requires` forwards the request's declared capability
        /// (`"tools"`, …) to the engine's capability gate.
        #[allow(clippy::too_many_arguments)] // flat params mirror the stream siblings; a params struct would only wrap them
        pub fn generate_reasoning_stream_with_model_meta(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
            on_meta: &dyn Fn(&str),
            requires: Option<&str>,
        ) -> String {
            Self::generate_reasoning_stream_with_model_meta_chat(
                prompt, workspace, on_chunk, model, on_meta, requires, None,
            )
        }

        /// Streaming reasoning with the complete caller conversation for a
        /// local engine that can render the model's own template. The prose
        /// prompt remains the fallback for older engines and cloud providers.
        #[allow(clippy::too_many_arguments)]
        pub fn generate_reasoning_stream_with_model_meta_chat(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
            on_meta: &dyn Fn(&str),
            requires: Option<&str>,
            chat: Option<Value>,
        ) -> String {
            Self::stream(
                prompt, workspace, on_chunk, model, on_meta, false, requires, chat,
            )
        }

        /// Streaming reasoning that never takes a reflex tier — for
        /// synthesis prompts (a mission's final answer, swarm consensus),
        /// which a trained Tier-0 could otherwise answer with a bare
        /// `ACTION:` line, and which Tier-1's 0.5B model should not write.
        pub fn generate_reasoning_stream_deep(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
        ) -> String {
            Self::stream(prompt, workspace, on_chunk, None, &|_| {}, true, None, None)
        }

        /// Deep streaming reasoning with a caller conversation.
        #[allow(clippy::too_many_arguments)]
        pub fn generate_reasoning_stream_deep_meta_chat(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
            on_meta: &dyn Fn(&str),
            chat: Option<Value>,
        ) -> String {
            Self::stream(
                prompt, workspace, on_chunk, model, on_meta, true, None, chat,
            )
        }

        #[allow(clippy::too_many_arguments)] // private helper behind the two public stream entry points; a params struct would only wrap them
        fn stream(
            prompt: &str,
            workspace: &Path,
            on_chunk: &dyn Fn(String),
            model: Option<&str>,
            on_meta: &dyn Fn(&str),
            deep: bool,
            requires: Option<&str>,
            chat: Option<Value>,
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
                    "deep": deep,
                    "requires": requires,
                    "chat": chat,
                    "on_demand_engine_start": crate::susi_core::provider::OnDemandEngineStart::allowed(),
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
            Self::deep(prompt, workspace, None, None, None, None)
        }

        pub fn generate_reasoning_deep_with_model(
            prompt: &str,
            workspace: &Path,
            model: &str,
        ) -> String {
            Self::deep(prompt, workspace, Some(model), None, None, None)
        }

        /// Deep reasoning with the request's declared capability
        /// (`"tools"`, …) — the recovery ladder threads `susi.requires`
        /// here so non-streaming missions apply the same capability gate
        /// and tool-schema injection the streaming path does.
        pub fn generate_reasoning_deep_full(
            prompt: &str,
            workspace: &Path,
            model: Option<&str>,
            requires: Option<&str>,
        ) -> String {
            Self::generate_reasoning_deep_full_chat(prompt, workspace, model, requires, None)
        }

        /// Non-streaming deep reasoning with the complete caller
        /// conversation, preserving the plain prompt fallback.
        pub fn generate_reasoning_deep_full_chat(
            prompt: &str,
            workspace: &Path,
            model: Option<&str>,
            requires: Option<&str>,
            chat: Option<Value>,
        ) -> String {
            Self::deep(prompt, workspace, model, None, requires, chat)
        }

        /// Shared non-streaming inference request: `model` selects the
        /// caller-named model, `requires` forwards the declared
        /// capability into the engine's local-rung gate.
        #[allow(clippy::too_many_arguments)]
        fn deep(
            prompt: &str,
            workspace: &Path,
            model: Option<&str>,
            min_complexity: Option<&str>,
            requires: Option<&str>,
            chat: Option<Value>,
        ) -> String {
            let topic = if model.is_some() {
                topics::GEMI_INFER_GENERATE_DEEP_MODEL
            } else {
                topics::GEMI_INFER_GENERATE_DEEP
            };
            req_ok(
                topic,
                json!({
                    "prompt": prompt,
                    "workspace": workspace.display().to_string(),
                    "model": model,
                    "min_complexity": min_complexity,
                    "requires": requires,
                    "chat": chat,
                    "on_demand_engine_start": crate::susi_core::provider::OnDemandEngineStart::allowed(),
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
                json!({
                    "text": text,
                    "model": model,
                    "on_demand_engine_start": crate::susi_core::provider::OnDemandEngineStart::allowed(),
                }),
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
            Self::deep(prompt, workspace, None, Some(complexity), None, None)
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

    /// The unified operator state (`susi top --json`) for the repository
    /// containing `workspace`.
    pub fn operator_snapshot(workspace: &Path) -> Result<Value, String> {
        req(
            topics::GEMI_OPERATOR_SNAPSHOT,
            json!({ "workspace": workspace.display().to_string() }),
        )
    }

    /// `susi ecosystem start <id>`: the engine's start message.
    pub fn ecosystem_start(id: &str) -> Result<String, String> {
        let v = req(topics::GEMI_ECOSYSTEM_START, json!({ "id": id }))?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v.get("message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// `susi ecosystem kb <action>`: `query` carries `action` (`list`,
    /// `show`, `search` or `check`) and that action's filters.
    pub fn ecosystem_kb(query: Value) -> Result<Value, String> {
        req(topics::GEMI_ECOSYSTEM_KB, query)
    }

    /// `susi ecosystem explain <id> [--dir]`.
    pub fn ecosystem_explain(id: &str, dir: Option<&Path>) -> Result<Value, String> {
        req(
            topics::GEMI_ECOSYSTEM_EXPLAIN,
            json!({ "id": id, "dir": dir.map(|d| d.display().to_string()) }),
        )
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

    /// Solve a mission while carrying the caller's cooperative cancellation
    /// flag through the watched PlaneBus request and into the GAWD handler.
    pub fn solve_mission_cancellable(
        intent: &str,
        workspace: &Path,
        version: &str,
        cancel: crate::progress::Cancel,
    ) -> String {
        crate::progress::with_cancel(Some(cancel), || {
            req_watched(
                topics::GAWD_SOLVE,
                json!({
                    "intent": intent,
                    "workspace": workspace.display().to_string(),
                    "version": version,
                    "model": null
                }),
            )
        })
        .unwrap_or_else(|e| json!({ "error": e }))
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
    }

    /// Solve a mission for an ACP/A2A caller, preserving the active cancel
    /// scope and refusing an empty or unreachable GAWD answer.
    pub fn solve_mission_for_acp(
        intent: &str,
        workspace: &Path,
        version: &str,
    ) -> Result<String, String> {
        let answer = crate::progress::current_cancel().map_or_else(
            || solve_mission(intent, workspace, version),
            |cancel| solve_mission_cancellable(intent, workspace, version, cancel),
        );
        if answer.trim().is_empty() {
            Err("mission produced no result (GAWD plane unreachable or empty answer)".to_string())
        } else {
            Ok(answer)
        }
    }

    /// The user's own mission, as `susi <goal>` runs it in process, for a
    /// caller that forwards the goal to the daemon (`mission_run`). The
    /// handler may start a stopped local engine on demand, and the answer
    /// carries the mission's status and the CLI exit code (0 only for an
    /// explicitly successful mission), never an outcome guessed from prose.
    ///
    /// When this thread's call is watched ([`crate::progress`]), the
    /// mission's progress lines come back over a bus stream while it runs
    /// and reach the watcher as they arrive.
    pub fn run_mission(
        intent: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
    ) -> Value {
        req_watched(
            topics::GAWD_MISSION_RUN,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
                "version": version,
                "model": model
            }),
        )
        .unwrap_or_else(|e| json!({ "error": e }))
    }

    /// Generative mission solve (`/v1/chat/completions`): the model's own
    /// output is the product, so recovery may self-cite an inference
    /// receipt minted for the provider call instead of demanding
    /// mission-captured tool evidence a chat goal never produces.
    /// `requires` carries the request's declared capability (`"tools"`,
    /// …) into the mission's local rungs — the same gate the streaming
    /// path applies; without it the contract was validated then dropped.
    pub fn solve_mission_generative(
        intent: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
        requires: Option<&str>,
    ) -> String {
        solve_mission_generative_with_chat(intent, workspace, version, model, requires, None)
    }

    /// Generative mission solve with the caller's normalized conversation.
    /// Non-local providers continue to receive the assembled prose intent;
    /// the chat payload is carried through the GAWD/GEMI local path.
    #[allow(clippy::too_many_arguments)]
    pub fn solve_mission_generative_with_chat(
        intent: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
        requires: Option<&str>,
        chat: Option<Value>,
    ) -> String {
        req_ok(
            topics::GAWD_SOLVE,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
                "version": version,
                "model": model,
                "generative": true,
                "requires": requires,
                "chat": chat
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

    /// Dispatch one structured, payload-free action through the same
    /// `gawd.audit.action` gate used by legacy callers.  The owning GAWD
    /// handler persists it through the existing signed audit logger, so this
    /// kernel facade never creates a second audit sink.
    pub fn audit_structured_action(
        action: &crate::audit_export::AuditAction,
        workspace: &Path,
    ) -> Result<(), String> {
        let action_value = serde_json::to_value(action).map_err(|e| e.to_string())?;
        let v = req(
            topics::GAWD_AUDIT_ACTION,
            json!({
                "action": action_value,
                "tool": action.action_kind.as_str(),
                "detail": action.target.id,
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

    /// Run the autonomous planning loop (`susi plan`) for `goal` in
    /// `workspace`, at most `max_steps` steps. Returns the plan summary
    /// (status, final answer, agents) or the mission's error.
    pub fn plan_autonomous(
        goal: &str,
        workspace: &Path,
        version: &str,
        max_steps: u32,
    ) -> Result<Value, String> {
        let v = req(
            topics::GAWD_PLAN_AUTONOMOUS,
            json!({
                "goal": goal,
                "workspace": workspace.display().to_string(),
                "version": version,
                "max_steps": max_steps,
            }),
        )?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v)
    }

    /// Ingest a natural-language intent into `workspace`'s evidence ledger
    /// (`susi pulse`). Returns the ingestion message.
    pub fn ingest_intent(intent: &str, workspace: &Path) -> Result<String, String> {
        let v = req(
            topics::GAWD_PULSE_INGEST,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
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

    fn call(topic: &str, payload: Value) -> Result<Value, String> {
        let v = req(topic, payload)?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v)
    }

    /// `susi audit` / `susi admin audit`: the compliance audit report for
    /// `workspace`.
    pub fn audit_compliance(workspace: &Path) -> Result<String, String> {
        let v = call(
            topics::GAWD_AUDIT_COMPLIANCE,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// `susi admin audit-evidence export --input --output`: the segment
    /// count and destination.
    pub fn audit_evidence_export(input: &Path, output: &Path) -> Result<Value, String> {
        call(
            topics::GAWD_AUDIT_EVIDENCE_EXPORT,
            json!({
                "input": input.display().to_string(),
                "output": output.display().to_string(),
            }),
        )
    }

    /// `susi substrate-report`: the compiled-constants substrate report.
    pub fn substrate_report(workspace: &Path) -> Result<String, String> {
        let v = call(
            topics::GAWD_SUBSTRATE_REPORT,
            json!({ "workspace": workspace.display().to_string() }),
        )?;
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }
}

// ── Host composition-root facade (`susi-daemon`) ──────────────────────────

/// Capabilities whose implementation lives in the `susi-daemon` crate (the
/// composition root that sees every plane). `susi-daemon` registers the
/// `daemon.` prefix in `composition::wire_plane_handlers`; the CLI calls the
/// same daemon library functions directly.
pub mod daemon {
    use super::*;

    /// A reply carrying `{"error": ..}` is a refusal, not a result.
    fn call(topic: &str, payload: Value) -> Result<Value, String> {
        let v = req(topic, payload)?;
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(e.to_string());
        }
        Ok(v)
    }

    /// `susi privacy mode <mode>`.
    pub fn privacy_mode(mode: &str) -> Result<Value, String> {
        call(topics::DAEMON_PRIVACY_MODE, json!({ "mode": mode }))
    }

    /// `susi ambient start` for `workspace`.
    pub fn ambient_start(workspace: &Path) -> Result<Value, String> {
        call(
            topics::DAEMON_AMBIENT_START,
            json!({ "workspace": workspace.display().to_string() }),
        )
    }

    /// `susi context-graph sample-os [--user]` for `workspace`.
    pub fn context_sample_os(workspace: &Path, user: Option<&str>) -> Result<Value, String> {
        call(
            topics::DAEMON_CONTEXT_SAMPLE_OS,
            json!({ "workspace": workspace.display().to_string(), "user": user }),
        )
    }

    /// `susi ecosystem scan` (`cached == false`) or `susi ecosystem status`
    /// (`cached == true`: the last persisted scan).
    pub fn ecosystem_scan(cached: bool) -> Result<Value, String> {
        call(topics::DAEMON_ECOSYSTEM_SCAN, json!({ "cached": cached }))
    }

    /// `susi substrate status` for `workspace`.
    pub fn substrate_status(workspace: &Path) -> Result<Value, String> {
        call(
            topics::DAEMON_SUBSTRATE_STATUS,
            json!({ "workspace": workspace.display().to_string() }),
        )
    }

    /// `susi crown verify|status` for `workspace`.
    pub fn crown_verify(workspace: &Path) -> Result<Value, String> {
        call(
            topics::DAEMON_CROWN_VERIFY,
            json!({ "workspace": workspace.display().to_string() }),
        )
    }

    /// `susi os provision <action>`: `payload` carries `action` (`probe`,
    /// `list` or `apply`), `kind` and, for `apply`, an absolute `manifest`.
    pub fn os_provision(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_OS_PROVISION, payload)
    }

    /// `daemon.providers` (zero-UI plan B4 group 3): `payload` carries
    /// `action` and that action's arguments.
    pub fn providers(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_PROVIDERS, payload)
    }

    /// `daemon.adapters` (zero-UI plan B4 group 7).
    pub fn adapters(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_ADAPTERS, payload)
    }

    /// `daemon.harness` (zero-UI plan B4 group 1).
    pub fn harness(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_HARNESS, payload)
    }

    /// `daemon.host`: host views, maintenance and daemon lifecycle.
    pub fn host(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_HOST, payload)
    }

    /// `susi services <action> <name>` (`start`, `stop` or `restart`).
    pub fn services_control(action: &str, name: &str) -> Result<Value, String> {
        call(
            topics::DAEMON_SERVICES_CONTROL,
            json!({ "action": action, "name": name }),
        )
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

    /// Run tool `name`. A watched call ([`crate::progress`]) gets the
    /// tool's progress lines back while it runs ([`super::req_watched`]).
    pub fn execute_tool(name: &str, args: &Value, workspace: &Path) -> EaiResult<String> {
        let v = super::req_watched(
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

    /// Tools that download a model. They run only on an explicit user or
    /// client request (the CLI, or an MCP client's `tools/call`), never
    /// because a model's reply, a mission plan or a DAG step named them:
    /// core never downloads a model by itself (CEO decision 2026-11-04,
    /// "No, ask first").
    pub const USER_REQUEST_ONLY_TOOLS: &[&str] = &["scout_model"];

    /// The user's own mission (`mission_run`, a forwarded `susi <goal>`).
    /// It may start a stopped local engine, which core does only when the
    /// user asks, so a model reply, a mission plan or a DAG step never runs
    /// it.
    pub const USER_MISSION_TOOLS: &[&str] = &["mission_run"];

    /// True when `name` is one of [`USER_REQUEST_ONLY_TOOLS`] or
    /// [`USER_MISSION_TOOLS`]: tools only an explicit user or client
    /// request may run.
    #[must_use]
    pub fn is_user_request_only(name: &str) -> bool {
        let name = name.trim();
        USER_REQUEST_ONLY_TOOLS
            .iter()
            .chain(USER_MISSION_TOOLS)
            .any(|tool| tool.eq_ignore_ascii_case(name))
    }

    /// Why an autonomous dispatcher refused a [`USER_REQUEST_ONLY_TOOLS`]
    /// call, naming the commands that download a model on request.
    #[must_use]
    pub fn user_request_only_refusal(name: &str) -> String {
        let name = name.trim();
        if USER_MISSION_TOOLS
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(name))
        {
            return format!(
                "`{name}` runs a mission as your own request and may start a stopped \
                 local engine, so only you or an MCP client may call it: a model reply, \
                 a mission plan or a DAG step cannot."
            );
        }
        format!(
            "`{}` downloads a model, and Susi never downloads one by itself: \
             it runs only when you ask. Run `susi os manage model install starter` \
             for the audited starter, or `susi scout-model <https url>` for a \
             specific model. Cloud models work without a download.",
            name
        )
    }

    /// Execute a tool that a model reply, a mission plan or a DAG step
    /// named, rather than the user. A [`USER_REQUEST_ONLY_TOOLS`] call is
    /// refused before it reaches the bus; every other tool runs exactly as
    /// [`execute_tool`] runs it.
    pub fn execute_autonomous_tool(
        name: &str,
        args: &Value,
        workspace: &Path,
    ) -> EaiResult<String> {
        if is_user_request_only(name) {
            return Err(EaiError::governance(user_request_only_refusal(name)));
        }
        execute_tool(name, args, workspace)
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

    #[test]
    fn autonomous_dispatch_refuses_model_download_tools_before_the_bus() {
        for name in ["scout_model", " SCOUT_MODEL "] {
            let error = tools::execute_autonomous_tool(
                name,
                &json!("https://huggingface.co/x/y/resolve/main/m.gguf"),
                Path::new("."),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("never downloads one by itself"), "{error}");
            assert!(
                error.contains("susi os manage model install starter"),
                "{error}"
            );
        }
        assert!(!tools::is_user_request_only("read_file"));
        assert!(!tools::is_user_request_only("verify_model_download_agent"));
        // A plan or model reply cannot start the user's own mission, which
        // may start a stopped local engine.
        assert!(tools::is_user_request_only(" Mission_Run "));
        assert!(!tools::is_user_request_only("susi_solve"));
        let error =
            tools::execute_autonomous_tool("mission_run", &json!({"goal": "x"}), Path::new("."))
                .unwrap_err()
                .to_string();
        assert!(error.contains("only you or an MCP client"), "{error}");
        assert!(!error.contains("downloads"), "{error}");
    }

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

    #[test]
    fn placement_constraints_default_absent_fields_and_refuse_malformed_ones() {
        let parse = |value: Value| {
            gemi::placement_constraints(value.as_object(), "")
                .map(|c| (c.requires, c.max_cost, c.allow_cloud))
        };
        assert_eq!(parse(json!({})), Ok((None, None, true)));
        assert_eq!(
            parse(json!({"requires": null, "max_cost": null, "allow_cloud": null})),
            Ok((None, None, true))
        );
        assert_eq!(
            parse(json!({"requires": "vision", "max_cost": 0.01, "allow_cloud": false})),
            Ok((Some("vision".to_string()), Some(0.01), false))
        );
        for bad in [
            json!({"requires": "telepathy"}),
            json!({"requires": "vision input"}),
            json!({"requires": 7}),
            json!({"max_cost": -1}),
            json!({"max_cost": "cheap"}),
            json!({"allow_cloud": "no"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad} must be refused");
        }
        assert_eq!(
            gemi::placement_constraints(json!({"allow_cloud": 1}).as_object(), "susi.")
                .unwrap_err(),
            "susi.allow_cloud must be a boolean"
        );
        assert!(gemi::validated_max_cost(Some(f64::NAN)).is_err());
        assert!(gemi::validated_max_cost(Some(f64::INFINITY)).is_err());
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

    /// Reports three lines through whatever sink the request installs.
    struct Reporter;

    impl PlaneHandler for Reporter {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let watched = progress_sink_for(&payload).is_some();
            crate::progress::with_sink(progress_sink_for(&payload), || {
                for line in ["one", "two", "three"] {
                    crate::progress::report(line);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
            Ok(json!({ "watched": watched }))
        }
    }

    /// A watched request carries its handler's progress lines back over the
    /// bus while it runs, in order; an unwatched one gets no stream.
    #[test]
    fn watched_requests_relay_progress_lines_across_the_bus() {
        let _g = wire();
        let topic = "test.progress.reporter";
        PlaneBus::global().register(topic, Arc::new(Reporter));
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = lines.clone();
        let sink: crate::progress::Sink = Arc::new(move |line: &str| {
            seen.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_string());
        });
        let watched = crate::progress::with_sink(Some(sink), || req_watched(topic, json!({})));
        assert_eq!(watched.unwrap()["watched"], true);
        assert_eq!(
            *lines.lock().unwrap_or_else(|e| e.into_inner()),
            ["one", "two", "three"]
        );
        let unwatched = req_watched(topic, json!({})).unwrap();
        assert_eq!(unwatched["watched"], false);
        assert!(progress_sink_for(&json!({ "progress_stream": "" })).is_none());
    }

    struct CancellableWorker;

    impl PlaneHandler for CancellableWorker {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            with_watched(&payload, || {
                crate::progress::report("phase 1");
                for _ in 0..80 {
                    if crate::progress::is_cancelled() {
                        return Ok(json!({ "status": "CANCELLED" }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(json!({ "status": "COMPLETED" }))
            })
        }
    }

    #[test]
    fn watched_requests_propagate_cancellation_across_the_bus() {
        let _g = wire();
        let topic = "test.progress.cancellable";
        PlaneBus::global().register(topic, Arc::new(CancellableWorker));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let trigger = cancel.clone();
        let sink: crate::progress::Sink = Arc::new(move |line: &str| {
            if line == "phase 1" {
                trigger.store(true, std::sync::atomic::Ordering::Release);
            }
        });
        let outcome = crate::progress::with_sink(Some(sink), || {
            crate::progress::with_cancel(Some(cancel), || req_watched(topic, json!({})))
        })
        .unwrap();
        assert_eq!(outcome["status"], "CANCELLED");
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
        let _ = gawd::solve_mission_generative("i", ws, "v", None, Some("tools"));
        let _ = gawd::run_mission("i", ws, "v", Some("m"));
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

    #[test]
    fn gemi_generate_reasoning_deep_propagates_on_demand_engine_start_across_worker_thread() {
        struct SpawnedOnDemandEcho;
        impl PlaneHandler for SpawnedOnDemandEcho {
            fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
                let flag = payload
                    .get("on_demand_engine_start")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let seen_on_worker = std::thread::spawn(move || {
                    let _guard =
                        crate::susi_core::provider::OnDemandEngineStart::from_bus_flag(flag);
                    crate::susi_core::provider::OnDemandEngineStart::allowed()
                })
                .join()
                .map_err(|_| "worker panicked".to_string())?;
                Ok(json!({
                    "ok": true,
                    "text": format!("allowed={seen_on_worker}")
                }))
            }
        }

        let _g = wire();
        let ws = Path::new(".");
        PlaneBus::global().register(
            topics::GEMI_INFER_GENERATE_DEEP,
            Arc::new(SpawnedOnDemandEcho),
        );

        let without_guard = gemi::GemiEngine::generate_reasoning_deep("probe", ws);
        assert_eq!(without_guard, "allowed=false");

        let with_guard = {
            let _on_demand = crate::susi_core::provider::OnDemandEngineStart::from_bus_flag(true);
            gemi::GemiEngine::generate_reasoning_deep("probe", ws)
        };
        assert_eq!(with_guard, "allowed=true");

        let after_drop = gemi::GemiEngine::generate_reasoning_deep("probe", ws);
        assert_eq!(after_drop, "allowed=false");
    }
}
