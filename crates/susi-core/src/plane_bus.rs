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
use susi_config::JsonFields as _;
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
    /// Served by Susi Commercial Apps (`susi top`); outside `gemi.`, so scoped sessions reach it.
    pub const OPERATOR_SNAPSHOT: &str = "ecosystem_hub.operator.snapshot.v1";
    /// The cluster roster and commit ledger behind GMCP's `peers_*` and `commits_*` tools.
    /// Served by Susi Commercial Apps (Fleet Command); outside the pillar prefixes.
    pub const FLEET_CLUSTER: &str = "fleet.cluster.v1";
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
    pub fn unregister(&self, topic: &str) -> bool {
        self.inner.unregister(topic)
    }
    pub fn request_local_prefix(&self, topic: &str, payload: Value) -> Result<Value, String> {
        self.inner.request_local_prefix(topic, payload)
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
    if let Some(scope) = crate::scoped_tools::current() {
        scope.check()?;
        if ["gawd.", "gemi.", "tools.", "agents."]
            .iter()
            .any(|prefix| topic.starts_with(prefix))
        {
            // The tool owner is an in-process capability, never an IPC payload.
            return PlaneBus::global().request_local_prefix(topic, payload);
        }
    }
    PlaneBus::global().request(topic, payload)
}
/// Interpret the workspace of an incoming plane request. Omitted or non-string
/// values use the process directory; explicit strings retain their native spelling.
pub fn workspace_path(payload: &Value) -> PathBuf {
    PathBuf::from(payload.field_str("workspace").unwrap_or("."))
}

/// The composition root returns refusals in the response error field.
fn call(topic: &str, payload: Value) -> Result<Value, String> {
    let response = req(topic, payload)?;
    if let Some(error) = response.field_str("error") {
        return Err(error.to_owned());
    }
    Ok(response)
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
                    if let (Some(sink), Some(line)) = (&sink, chunk.field_str("message")) {
                        sink(line);
                    }
                    if let Some(cs) = chunk.field_str(CANCEL_STREAM) {
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
        .field_str(PROGRESS_STREAM)
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
        .field_str(PROGRESS_STREAM)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        return call();
    };
    let initially_cancelled = payload.field_bool("cancelled").unwrap_or(false);
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
/// A GMCP cluster tool's `action` with its `args`, answered on
/// [`topics::FLEET_CLUSTER`] by Susi Commercial Apps.
pub fn fleet_cluster(action: &str, args: Value) -> Result<Value, String> {
    req(
        topics::FLEET_CLUSTER,
        json!({ "action": action, "args": args }),
    )
}
// ── The two call wrappers core itself uses; the other typed per-plane wrappers
// live with their callers in Susi Commercial Apps. ───────────────────────────
pub mod daemon {
    use super::*;
    /// `daemon.harness` (zero-UI plan B4 group 1).
    pub fn harness(payload: Value) -> Result<Value, String> {
        call(topics::DAEMON_HARNESS, payload)
    }
}
pub mod tools {
    use super::*;
    use crate::susi_error::{EaiError, EaiResult};
    /// Run tool `name`. A watched call ([`crate::progress`]) gets the
    /// tool's progress lines back while it runs ([`super::req_watched`]).
    pub fn execute_tool(name: &str, args: &Value, workspace: &Path) -> EaiResult<String> {
        let v = super::req_watched(
            topics::TOOLS_EXECUTE,
            json!({"name":name,"args":args,"workspace":workspace.display().to_string()}),
        )
        .map_err(EaiError::protocol)?;
        if let Some(e) = v.field_str("error") {
            return Err(super::remote_error(e));
        }
        Ok(v.field_str("text").unwrap_or("").to_string())
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
    //! Bus mechanics the typed call wrappers rely on: watched requests relay
    //! progress and cancellation, scoped tools stay local, streams round-trip.
    //! The per-plane wrappers and their marshal tests live in Susi Commercial
    //! Apps; a catch-all per prefix keeps these requests deterministic.
    use super::*;
    use std::sync::{Mutex, Once};
    static LOCK: Mutex<()> = Mutex::new(());
    static ONCE: Once = Once::new();
    static ISOLATED_HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    struct WireGuard {
        _env: susi_paths::test_env::EnvGuard,
        _env_lock: std::sync::MutexGuard<'static, ()>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }
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
    fn wire() -> WireGuard {
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env_lock = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = ISOLATED_HOME.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("susi-facade-bus-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            dir
        });
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", root);
        env.set("XDG_CACHE_HOME", root.join(".cache"));
        env.set("SUSI_HOME", root);
        ONCE.call_once(|| {
            let bus = PlaneBus::global();
            bus.register_prefix("gemi.", Arc::new(Echo));
            bus.register_prefix("gawd.", Arc::new(Echo));
            bus.register_prefix("agents.", Arc::new(EmptyList));
            bus.register_prefix("tools.", Arc::new(Echo));
            bus.register(topics::GEMI_INFER_EMBED, Arc::new(Embed));
        });
        WireGuard {
            _env: env,
            _env_lock: env_lock,
            _lock: lock,
        }
    }
    #[test]
    fn scoped_tools_pin_owner_requests_to_local_prefixes() {
        use crate::scoped_tools::{with_scope, ScopedTool, ScopedToolProvider, ToolScope};
        struct Owner;
        impl ScopedToolProvider for Owner {
            fn tools(&self) -> &[ScopedTool] {
                &[]
            }
            fn check(&self) -> Result<(), String> {
                Ok(())
            }
            fn call(
                &self,
                _: &str,
                _: &Value,
                _: &Path,
                _: Option<&str>,
                _: Option<crate::progress::Cancel>,
                _: crate::progress::Cancel,
            ) -> Result<String, String> {
                unreachable!()
            }
        }
        let _guard = wire();
        let bus = PlaneBus::global();
        let scope = ToolScope::new(
            Arc::new(Owner),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        for topic in [
            "gemi.scoped_probe",
            "gawd.scoped_probe",
            "agents.scoped_probe",
            "tools.scoped_probe",
        ] {
            bus.register(topic, Arc::new(Embed)); // Represents an exact IPC/remote route.
            assert!(req(topic, json!({})).unwrap().get("embedding").is_some());
            let local = with_scope(Some(scope.clone()), || req(topic, json!({}))).unwrap();
            assert!(
                local.get("embedding").is_none(),
                "{topic} escaped its owner process"
            );
        }
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
}
