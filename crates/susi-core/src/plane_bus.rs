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
use std::path::Path;
use std::sync::{Arc, OnceLock};
use susi_config::JsonFields as _;
/// Rebuild a handler's displayed error (`"Protocol Error: ..."`) as the
/// same `EaiError` kind instead of wrapping it in another layer, which
/// rendered "Protocol Error: Protocol Error: ..." and lost the real kind.
fn remote_error(display: &str) -> crate::susi_error::EaiError {
    crate::susi_error::rewrap_display(display)
}
/// Topic constants exercised by core socket dispatch; per-plane topic catalogs live in Susi Commercial Apps.
pub mod topics {
    pub const GEMI_INFER_EMBED: &str = "gemi.infer.embed";
    pub const TOOLS_EXECUTE: &str = "tools.registry.execute";
    pub const DAEMON_HARNESS: &str = "daemon.harness";
}
/// Handler for one or more topic prefixes (exact topic match first).
pub trait PlaneHandler: Send + Sync {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String>;
}
/// Process-wide plane bus — facade over [`IpcPlaneBus`].
pub struct PlaneBus {
    inner: Arc<IpcPlaneBus>,
}
impl PlaneBus {
    pub fn global() -> &'static PlaneBus {
        static BUS: OnceLock<PlaneBus> = OnceLock::new();
        BUS.get_or_init(|| PlaneBus {
            inner: IpcPlaneBus::global(),
        })
    }
    pub fn register(&self, topic: &str, handler: Arc<dyn PlaneHandler>) {
        self.inner.register(topic, handler);
    }
    pub fn unregister(&self, topic: &str) {
        self.inner.unregister(topic);
    }
    pub fn register_prefix(&self, prefix: &str, handler: Arc<dyn PlaneHandler>) {
        self.inner.register_prefix(prefix, handler);
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
            return PlaneBus::global().request_local_prefix(topic, payload);
        }
    }
    PlaneBus::global().request(topic, payload)
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
