//! IPC backend for `plane_bus` — the microkernel rendezvous.
//!
//! SUSI runs its planes as separate cell processes (`susi-gawd`,
//! `susi-gemi`, `susi-gmcp`, the universal/dsh cells, the daemon), each with
//! its own `PlaneBus::global()` `OnceLock`. `IpcPlaneBus` gives every process
//! the same wire semantics over loopback instead:
//!
//! - `register` / `register_prefix` lazily bind a `127.0.0.1:0`
//!   listener and publish the endpoint under `<cache>/bus/<pid>/` — a
//!   filesystem rendezvous scoped to the owning process (`std::process::id`),
//!   so parallel processes (daemon, CLI, test binaries) keep today's
//!   per-process bus isolation.
//! - `request` resolves a topic to its endpoint file and POSTs the JSON
//!   payload (`POST /handle` → `{ok|err}`); exact topic files are tried
//!   before prefix files, mirroring the in-process lookup order.
//! - `open_stream` embeds the caller's endpoint in the stream id
//!   (`ipc://<addr>/<id>`) so `stream_emit` reaches the right process from
//!   anywhere; the `/stream` endpoint feeds the local flume receiver.
//! - Stale endpoint files are pruned on connect failure; dead pid dirs are
//!   swept on init where `/proc` is available.

use crate::susi_core::plane_bus::PlaneHandler;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use susi_paths::SusiDirs;

/// Connect timeout: the rendezvous only ever targets this host.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Cap on request head bytes read while hunting for `\r\n\r\n`.
const MAX_HEAD: usize = 64 * 1024;
/// Cap on an IPC request body; larger declared lengths are refused unread.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// Concurrent bus connections served at once; excess callers get a 503.
const MAX_CONNECTIONS: usize = 256;

/// One claimed connection slot, released on drop.
struct ConnSlot(Arc<std::sync::atomic::AtomicUsize>);

impl ConnSlot {
    fn claim(live: &Arc<std::sync::atomic::AtomicUsize>, max: usize) -> Option<Self> {
        use std::sync::atomic::Ordering;
        live.try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < max).then_some(n + 1)
        })
        .ok()
        .map(|_| Self(Arc::clone(live)))
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
/// Header carrying the per-substrate bus secret on every IPC request.
const BUS_KEY_HEADER: &str = "X-Susi-Bus-Key";
/// Prefix of the error a copy answers for a topic it does not serve.
const NO_HANDLER: &str = "plane bus: no handler for topic";
/// Response field a serving copy sets — beside the `err` older callers
/// read — when its dispatch has no handler for the topic at all.
const MISS_FIELD: &str = "miss";

fn no_handler(topic: &str) -> String {
    format!("{NO_HANDLER} '{topic}' (composition root must register planes)")
}

type HandlerMap = Arc<RwLock<HashMap<String, Arc<dyn PlaneHandler>>>>;
type StreamMap = Arc<RwLock<HashMap<String, std::sync::mpsc::Sender<Value>>>>;

/// Monotonic process-local copy identifier so multiple in-process test buses
/// sharing a single directory publish distinct `routes.<id>.json` /
/// `caps.<id>.json` manifests while a real `<cache>/bus/<pid>` owner publishes
/// `routes.json` and `caps.json`.
static NEXT_COPY_ID: AtomicU64 = AtomicU64::new(1);

/// Process-scoped IPC plane bus — same method surface as
/// [`crate::susi_core::plane_bus::PlaneBus`]. Every cell process backs its
/// `PlaneBus` with this, so a handler registered by one module resolves from
/// every other module and from sibling processes over the rendezvous.
pub struct IpcPlaneBus {
    handlers: HandlerMap,
    prefixes: HandlerMap,
    streams: StreamMap,
    stream_seq: AtomicU64,
    rendezvous: PathBuf,
    endpoint: OnceLock<SocketAddr>,
    /// Shared secret every `/handle` and `/stream` request must carry.
    secret: Arc<str>,
    /// Exclusive lock on `<rendezvous>/owner.lock`, held for this copy's
    /// lifetime: the OS releases it when the process exits however it
    /// exits, so a sweep can tell a dead copy's dir from a live one without
    /// trusting a pid that may have been reused.
    _owner: Option<std::fs::File>,
    copy_id: u64,
    batch_depth: std::sync::atomic::AtomicUsize,
    dirty_routes: std::sync::atomic::AtomicBool,
}

impl Default for IpcPlaneBus {
    fn default() -> Self {
        Self::new()
    }
}

impl IpcPlaneBus {
    /// Bus for this process: rendezvous under `SusiDirs::cache_dir()/bus/<pid>`.
    pub fn new() -> Self {
        let dir = SusiDirs::cache_dir()
            .join("bus")
            .join(std::process::id().to_string());
        Self::with_rendezvous(dir)
    }

    /// The shared bus for this process: every module routes through it, so a
    /// registration from `registry_ipc` is reachable via `plane_bus` lookups
    /// and vice versa.
    pub fn global() -> Arc<Self> {
        static BUS: OnceLock<Arc<IpcPlaneBus>> = OnceLock::new();
        Arc::clone(BUS.get_or_init(|| Arc::new(Self::new())))
    }

    /// Explicit rendezvous dir — tests give each "process" its own dir tree
    /// without mutating process env.
    pub fn with_rendezvous(rendezvous: PathBuf) -> Self {
        // Own the dir before sweeping, so a concurrent sweep by another
        // process never mistakes it for a dead copy's.
        let owner = claim_rendezvous(&rendezvous);
        if owner.is_some() {
            spawn_sweep(&rendezvous);
        }
        let secret = bus_secret(rendezvous.parent()).into();
        let copy_id = NEXT_COPY_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
            prefixes: Arc::new(RwLock::new(HashMap::new())),
            streams: Arc::new(RwLock::new(HashMap::new())),
            stream_seq: AtomicU64::new(0),
            rendezvous,
            endpoint: OnceLock::new(),
            secret,
            _owner: owner,
            copy_id,
            batch_depth: std::sync::atomic::AtomicUsize::new(0),
            dirty_routes: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// This copy's rendezvous dir — capability metadata and other
    /// process-scoped IPC state share the same `<cache>/bus/<pid>` tree.
    pub fn rendezvous(&self) -> &Path {
        &self.rendezvous
    }

    /// Single route manifest path for this copy: `<rendezvous>/routes.json`
    /// when holding the `<cache>/bus/<pid>` owner lock, or
    /// `<rendezvous>/routes.<copy_id>.json` for shared in-process test dirs.
    pub fn routes_file(&self) -> PathBuf {
        if self._owner.is_some() {
            self.rendezvous.join("routes.json")
        } else {
            self.rendezvous
                .join(format!("routes.{}.json", self.copy_id))
        }
    }

    /// Single capability manifest path paired with [`Self::routes_file`].
    pub fn caps_file(&self) -> PathBuf {
        if self._owner.is_some() {
            self.rendezvous.join("caps.json")
        } else {
            self.rendezvous.join(format!("caps.{}.json", self.copy_id))
        }
    }

    /// Already-bound listener tag without triggering lazy listener bind.
    pub(crate) fn bound_endpoint_tag(&self) -> String {
        self.endpoint
            .get()
            .map(ToString::to_string)
            .unwrap_or_default()
    }

    /// Defer route-manifest disk writes until the matching [`Self::end_batch`].
    pub fn begin_batch(&self) {
        self.batch_depth.fetch_add(1, Ordering::AcqRel);
    }

    /// Complete a batch started by [`Self::begin_batch`], flushing at most one
    /// atomic route manifest when the outermost batch exits dirty.
    pub fn end_batch(&self) {
        let prev = self.batch_depth.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 && self.dirty_routes.swap(false, Ordering::AcqRel) {
            self.flush_routes();
        }
    }

    /// Own rendezvous dir first, then every other live numeric-named pid
    /// dir under `<cache>/bus/` (sorted) — the cross-process discovery
    /// set. Reads scan all dirs so *separate processes* resolve each
    /// other's registrations; writes always stay in the owning process's
    /// dir so `sweep_dead_processes` keeps ownership/liveness tracking.
    pub fn rendezvous_dirs(&self) -> Vec<PathBuf> {
        sibling_pid_dirs(&self.rendezvous)
    }

    /// This copy's callback endpoint; binds the listener on first use.
    /// `None` if loopback bind failed (registrations then stay local-only).
    /// This copy's listener address (bound lazily on first registration or
    /// stream open). `None` if loopback bind failed.
    pub fn endpoint(&self) -> Option<SocketAddr> {
        if let Some(a) = self.endpoint.get() {
            return Some(*a);
        }
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).ok()?;
        let addr = listener.local_addr().ok()?;
        if self.endpoint.set(addr).is_ok() {
            self.spawn_accept(listener);
            Some(addr)
        } else {
            // Lost a bind race within this copy; use the winner's endpoint.
            self.endpoint.get().copied()
        }
    }

    fn spawn_accept(&self, listener: TcpListener) {
        let handlers = Arc::clone(&self.handlers);
        let prefixes = Arc::clone(&self.prefixes);
        let streams = Arc::clone(&self.streams);
        let secret = Arc::clone(&self.secret);
        std::thread::spawn(move || {
            // Threads are spawned before the bus key is checked, so any
            // local process could otherwise pin unbounded threads (each up
            // to the 30s socket timeout) with idle connections.
            let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let Some(slot) = ConnSlot::claim(&live, MAX_CONNECTIONS) else {
                    respond(
                        &mut conn,
                        "503 Service Unavailable",
                        r#"{"err":"bus busy"}"#,
                    );
                    continue;
                };
                let handlers = Arc::clone(&handlers);
                let prefixes = Arc::clone(&prefixes);
                let streams = Arc::clone(&streams);
                let secret = Arc::clone(&secret);
                std::thread::spawn(move || {
                    let _slot = slot;
                    serve_conn(conn, handlers, prefixes, streams, &secret);
                });
            }
        });
    }

    /// Atomically publish or remove this copy's single route manifest.
    fn flush_routes(&self) {
        let mut topics: Vec<String> = self
            .handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let mut prefixes: Vec<String> = self
            .prefixes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let path = self.routes_file();
        if topics.is_empty() && prefixes.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let Some(ep) = self.endpoint() else { return };
        topics.sort();
        prefixes.sort();
        let body = json!({
            "endpoint": ep.to_string(),
            "topics": topics,
            "prefixes": prefixes,
        })
        .to_string();
        // Rendezvous files die with this copy; see `registry_ipc` for why
        // per-process endpoint metadata is replaced atomically without fsync.
        let _ = crate::susi_config::atomic_replace_unsynced(&path, body.as_bytes());
    }

    fn mark_or_flush_routes(&self) {
        if self.batch_depth.load(Ordering::Acquire) > 0 {
            self.dirty_routes.store(true, Ordering::Release);
        } else {
            self.flush_routes();
        }
    }

    /// Ordered remote candidates: every exact topic match across live
    /// rendezvous dirs first (single-file `routes*.json` manifests plus legacy
    /// `topics/` files), then every prefix match whose key prefixes `topic` —
    /// exact registrations outrank prefix handlers process-wide.
    /// Returns `(file, endpoint)` pairs so stale files can be pruned on
    /// connect failure or squatted port reuse.
    fn candidates(&self, topic: &str) -> Vec<(PathBuf, SocketAddr)> {
        let dirs = self.rendezvous_dirs();
        let manifests_by_dir: Vec<(&PathBuf, Vec<(PathBuf, RouteManifest)>)> = dirs
            .iter()
            .map(|dir| (dir, read_dir_route_manifests(dir)))
            .collect();
        let mut out = Vec::new();
        for (dir, manifests) in &manifests_by_dir {
            for (path, m) in manifests {
                if m.topics.iter().any(|t| t == topic) {
                    out.push((path.clone(), m.endpoint));
                }
            }
            let exact = dir.join("topics").join(enc(topic));
            if let Some((ep, _)) = read_endpoint_key(&exact) {
                out.push((exact, ep));
            }
        }
        for (dir, manifests) in &manifests_by_dir {
            for (path, m) in manifests {
                if m.prefixes.iter().any(|p| topic.starts_with(p.as_str())) {
                    out.push((path.clone(), m.endpoint));
                }
            }
            let pdir = dir.join("prefixes");
            if let Ok(rd) = std::fs::read_dir(&pdir) {
                for entry in rd.flatten() {
                    let path = entry.path();
                    if let Some((ep, _)) = read_endpoint_key(&path)
                        .filter(|(_, prefix)| topic.starts_with(prefix.as_str()))
                    {
                        out.push((path, ep));
                    }
                }
            }
        }
        out
    }

    /// Local prefix match (rendezvous file may be missing).
    fn local_prefix(&self, topic: &str) -> Option<Arc<dyn PlaneHandler>> {
        self.prefixes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|e| topic.starts_with(e.0.as_str()))
            .map(|(_, v)| Arc::clone(v))
    }

    /// `POST /handle` to a remote copy. `Remote::Unreachable` = transport
    /// failure; `Remote::Miss` = live squatter on a reused port with no
    /// handler for `topic`.
    fn remote(ep: SocketAddr, topic: &str, payload: &Value, secret: &str) -> Remote {
        let Ok(body) = serde_json::to_string(&json!({
            "topic": topic,
            "payload": payload,
        })) else {
            return Remote::Answered(Err("bus request failed to serialize".to_string()));
        };
        let Some(resp) = post(ep, "/handle", &body, secret) else {
            return Remote::Unreachable;
        };
        let v: Value = match serde_json::from_str(&resp) {
            Ok(v) => v,
            Err(e) => {
                return Remote::Answered(Err(format!("malformed bus response: {e}")));
            }
        };
        // The endpoint is alive but its dispatch found no handler at all for
        // the topic: the file that pointed here is stale — its owner died
        // and another copy now holds the port (or the owner just unregistered
        // `topic`). Only the structural marker counts: a handler's own error
        // is an answer whatever its text, even one that relays a nested
        // request's "no handler" message.
        if v.get(MISS_FIELD).is_some() {
            return Remote::Miss;
        }
        if let Some(e) = v.get("err").and_then(|e| e.as_str()) {
            Remote::Answered(Err(e.to_string()))
        } else if let Some(ok) = v.get("ok") {
            Remote::Answered(Ok(ok.clone()))
        } else {
            Remote::Answered(Err("malformed bus response".to_string()))
        }
    }

    pub fn register(&self, topic: &str, handler: Arc<dyn PlaneHandler>) {
        self.handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(topic.to_string(), handler);
        self.mark_or_flush_routes();
    }

    pub fn register_prefix(&self, prefix: &str, handler: Arc<dyn PlaneHandler>) {
        self.prefixes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(prefix.to_string(), handler);
        self.mark_or_flush_routes();
    }

    /// Remove an exact-topic registration made by this copy. Updates this
    /// copy's route manifest and removes any legacy per-topic file owned by
    /// this copy's listener — another copy's registration is left alone.
    pub fn unregister(&self, topic: &str) -> bool {
        let removed = self
            .handlers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(topic)
            .is_some();
        if removed {
            self.mark_or_flush_routes();
        }
        let file = self.rendezvous.join("topics").join(enc(topic));
        let owns_file = read_endpoint_key(&file)
            .is_some_and(|(ep, _)| self.endpoint.get().is_some_and(|e| *e == ep));
        if owns_file {
            let _ = std::fs::remove_file(&file);
        }
        removed
    }

    pub fn request(&self, topic: &str, payload: Value) -> Result<Value, String> {
        // Fast path: a handler registered in this copy stays in-process.
        if let Some(h) = self
            .handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(topic)
            .map(Arc::clone)
        {
            return invoke(h, topic, payload);
        }
        for (file, ep) in self.candidates(topic) {
            match Self::remote(ep, topic, &payload, &self.secret) {
                Remote::Answered(res) => return res,
                Remote::Unreachable => prune_stale_route_file(&file, ep, None),
                Remote::Miss => prune_stale_route_file(&file, ep, Some(topic)),
            }
        }
        if let Some(h) = self.local_prefix(topic) {
            return invoke(h, topic, payload);
        }
        Err(no_handler(topic))
    }

    pub fn publish(&self, topic: &str, payload: Value) {
        let _ = self.request(topic, payload);
    }

    /// Allocate a stream channel; the id embeds this copy's endpoint so
    /// `stream_emit` from any peer process reaches `rx` over loopback.
    pub fn open_stream(&self) -> (String, std::sync::mpsc::Receiver<Value>) {
        let id = format!(
            "{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            self.stream_seq.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = std::sync::mpsc::channel();
        self.streams
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);
        let sid = match self.endpoint() {
            Some(ep) => format!("ipc://{ep}/{id}"),
            None => id,
        };
        (sid, rx)
    }

    pub fn stream_emit(&self, stream_id: &str, chunk: Value) {
        if let Some(rest) = stream_id.strip_prefix("ipc://") {
            let Some((addr, id)) = rest.split_once('/') else {
                return;
            };
            if self.endpoint.get().is_some_and(|e| addr == e.to_string()) {
                if let Some(tx) = self
                    .streams
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                {
                    let _ = tx.send(chunk);
                }
            } else if let Ok(ep) = addr.parse::<SocketAddr>() {
                let body = json!({ "id": id, "chunk": chunk }).to_string();
                let _ = post(ep, "/stream", &body, &self.secret);
            }
        } else if let Some(tx) = self
            .streams
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(stream_id)
        {
            let _ = tx.send(chunk);
        }
    }

    pub fn stream_close(&self, stream_id: &str) {
        let id = stream_id
            .strip_prefix("ipc://")
            .and_then(|r| r.split_once('/').map(|(_, id)| id))
            .unwrap_or(stream_id);
        self.streams
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn is_wired(&self, topic: &str) -> bool {
        self.handlers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(topic)
            || self.local_prefix(topic).is_some()
            || !self.candidates(topic).is_empty()
    }
}

enum Remote {
    Answered(Result<Value, String>),
    Unreachable,
    Miss,
}

fn invoke(h: Arc<dyn PlaneHandler>, topic: &str, payload: Value) -> Result<Value, String> {
    let h = Arc::clone(&h);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        h.handle(topic, payload)
    }))
    .unwrap_or_else(|_| Err("plane bus: handler panicked".to_string()))
}

/// Topic/prefix → filename. Dots are already filename-safe; encode the rest.
pub(crate) fn enc(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The bus secret shared by every process of one substrate: `<bus>/secret`,
/// created once (exclusive create, owner-only on Unix) and read by later
/// processes. Without a bus dir the secret is process-local. Without OS
/// entropy no secret is minted: the empty secret matches no presented key,
/// so the bus refuses remote calls instead of accepting a guessable one.
fn bus_secret(bus_dir: Option<&Path>) -> String {
    let fresh = || {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).ok()?;
        Some(raw.iter().map(|b| format!("{b:02x}")).collect::<String>())
    };
    let Some(dir) = bus_dir else {
        return fresh().unwrap_or_default();
    };
    let path = dir.join("secret");
    let read = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() == 64)
    };
    if let Some(existing) = read() {
        return existing;
    }
    let _ = std::fs::create_dir_all(dir);
    let Some(candidate) = fresh() else {
        return read().unwrap_or_default();
    };
    // Owner-only from birth: `0600` on Unix; on Windows a protected DACL for
    // the process token's user applied inside the exclusive create itself
    // (no `icacls` process, which opened a console window from the
    // windowless daemon). No other handle can be opened against a wider
    // list before the key is written, and a failure creates nothing.
    #[cfg(windows)]
    let created = winapi::acl::create_new_owner_only(&path);
    #[cfg(not(windows))]
    let created = {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&path)
    };
    match created {
        Ok(mut f) => {
            if f.write_all(candidate.as_bytes()).is_ok() {
                return candidate;
            }
        }
        // Windows: the owner-only file could not be made (no token user, no
        // list, or the create itself failed). The key is withheld: an empty
        // key matches no presented key, so the bus refuses every client
        // rather than run on a key it could not store owner-only.
        #[cfg(windows)]
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return String::new(),
        Err(_) => {}
    }
    // Lost the create race (or a partial write is being completed): the
    // winner's value is authoritative once it lands.
    for _ in 0..50 {
        if let Some(existing) = read() {
            return existing;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    candidate
}

/// Constant-time comparison of a presented bus key; empty never matches.
fn key_matches(presented: &str, secret: &str) -> bool {
    let (a, b) = (presented.as_bytes(), secret.as_bytes());
    !b.is_empty()
        && a.len() == b.len()
        && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct RouteManifest {
    endpoint: SocketAddr,
    topics: Vec<String>,
    prefixes: Vec<String>,
}

fn is_routes_manifest_name(name: &str) -> bool {
    name == "routes.json" || (name.starts_with("routes.") && name.ends_with(".json"))
}

fn is_rendezvous_manifest_name(name: &str) -> bool {
    is_routes_manifest_name(name)
        || name == "caps.json"
        || (name.starts_with("caps.") && name.ends_with(".json"))
}

fn read_routes_manifest(path: &Path) -> Option<RouteManifest> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let endpoint = v.get("endpoint")?.as_str()?.parse().ok()?;
    let topics = v
        .get("topics")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let prefixes = v
        .get("prefixes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Some(RouteManifest {
        endpoint,
        topics,
        prefixes,
    })
}

fn read_dir_route_manifests(dir: &Path) -> Vec<(PathBuf, RouteManifest)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            is_routes_manifest_name(&name).then(|| e.path())
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let m = read_routes_manifest(&p)?;
            Some((p, m))
        })
        .collect()
}

fn read_endpoint_key(path: &Path) -> Option<(SocketAddr, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let ep = v.get("endpoint")?.as_str()?.parse().ok()?;
    let key = v.get("key")?.as_str()?.to_string();
    Some((ep, key))
}

/// Remove a stale route file (`routes*.json` or legacy `topics/` / `prefixes/`)
/// only when it still points to `ep` and, on a live-endpoint `Miss`, still
/// advertises `missed_topic` (preventing concurrent `unregister` updates on a
/// live multi-topic `routes.json` from unlinking the updated manifest).
fn prune_stale_route_file(path: &Path, ep: SocketAddr, missed_topic: Option<&str>) {
    let is_manifest = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_routes_manifest_name);
    let should_remove = if is_manifest {
        read_routes_manifest(path).is_some_and(|m| {
            m.endpoint == ep
                && missed_topic.is_none_or(|topic| {
                    m.topics.iter().any(|t| t == topic)
                        || m.prefixes.iter().any(|p| topic.starts_with(p.as_str()))
                })
        })
    } else {
        read_endpoint_key(path).is_some_and(|(file_ep, key)| {
            file_ep == ep
                && missed_topic.is_none_or(|topic| key == topic || topic.starts_with(&key))
        })
    };
    if should_remove {
        let _ = std::fs::remove_file(path);
    }
}

/// POST one JSON body over HTTP/1.0; returns the response body.
fn post(addr: SocketAddr, path: &str, body: &str, secret: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let req = format!(
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n{BUS_KEY_HEADER}: {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    buf.split("\r\n\r\n").nth(1).map(str::to_string)
}

/// Read one HTTP/1.0 request: headers, then exactly Content-Length bytes.
/// Returns `(path, presented bus key, body)`.
/// The body is only read once the bus key matches `secret` and its declared
/// length is within [`MAX_BODY`], so an unauthenticated peer cannot make
/// the server buffer arbitrary data.
fn read_request(stream: &mut TcpStream, secret: &str) -> Option<(String, String, String)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let path = head.split_whitespace().nth(1)?.to_string();
    let content_len = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    let bus_key_prefix = format!("{}:", BUS_KEY_HEADER.to_ascii_lowercase());
    let presented = head
        .lines()
        .find_map(|l| {
            let lower = l.to_ascii_lowercase();
            lower
                .starts_with(&bus_key_prefix)
                .then(|| l[bus_key_prefix.len()..].trim().to_string())
        })
        .unwrap_or_default();
    if !key_matches(&presented, secret) || content_len > MAX_BODY {
        return Some((path, presented, String::new()));
    }
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);
    Some((path, presented, String::from_utf8_lossy(&body).to_string()))
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = stream.write_all(
        format!(
            "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .as_bytes(),
    );
}

fn serve_conn(
    mut conn: TcpStream,
    handlers: HandlerMap,
    prefixes: HandlerMap,
    streams: StreamMap,
    secret: &str,
) {
    // Each connection holds a thread: a peer that never finishes its
    // request (or never reads the answer) must not pin it forever.
    let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(30)));
    let Some((path, presented, body)) = read_request(&mut conn, secret) else {
        return;
    };
    // Loopback is reachable by every local user and by web pages (no-CORS
    // POSTs); only the substrate owner's processes can read the secret.
    if !key_matches(&presented, secret) {
        respond(&mut conn, "403 Forbidden", r#"{"err":"bus key required"}"#);
        return;
    }
    match path.as_str() {
        "/handle" => {
            let resp = match serde_json::from_str::<Value>(&body) {
                Ok(v) => {
                    let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("");
                    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
                    match dispatch(&handlers, &prefixes, topic, payload) {
                        Some(Ok(v)) => json!({ "ok": v }),
                        Some(Err(e)) => json!({ "err": e }),
                        // No handler here at all: say so structurally, so a
                        // caller can tell a stale endpoint file from a
                        // handler's own error. `err` stays for older callers.
                        None => json!({ "err": no_handler(topic), MISS_FIELD: topic }),
                    }
                }
                Err(_) => json!({ "err": "malformed bus request" }),
            };
            respond(&mut conn, "200 OK", &resp.to_string());
        }
        "/stream" => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if let Some(tx) = streams.read().unwrap_or_else(|e| e.into_inner()).get(id) {
                    let _ = tx.send(v.get("chunk").cloned().unwrap_or(Value::Null));
                }
            }
            respond(&mut conn, "200 OK", "{}");
        }
        _ => respond(&mut conn, "404 Not Found", "{}"),
    }
}

/// Serving-side dispatch: exact topic match first, then prefix fallback.
/// `None` when this copy has no handler for `topic` at all — a dispatch
/// miss, distinct from any result a handler returns.
fn dispatch(
    handlers: &HandlerMap,
    prefixes: &HandlerMap,
    topic: &str,
    payload: Value,
) -> Option<Result<Value, String>> {
    if let Some(h) = handlers
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(topic)
        .map(Arc::clone)
    {
        return Some(invoke(h, topic, payload));
    }
    let h = prefixes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|e| topic.starts_with(e.0.as_str()))
        .map(|(_, v)| Arc::clone(v))?;
    Some(invoke(h, topic, payload))
}

/// `own` pid dir first, then every other numeric-named sibling dir under
/// `<cache>/bus/` sorted by name — the cross-process discovery set. No
/// liveness check here: dead pids are swept at bus init
/// (`sweep_dead_processes`) and dead endpoints are pruned on connect
/// failure, so a stale dir only costs one failed connect attempt.
pub fn sibling_pid_dirs(own: &Path) -> Vec<PathBuf> {
    let mut out = vec![own.to_path_buf()];
    let Some(bus_dir) = own.parent() else {
        return out;
    };
    let mut siblings: Vec<PathBuf> = std::fs::read_dir(bus_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    *p != *own
                        && p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.parse::<u64>().is_ok())
                })
                .collect()
        })
        .unwrap_or_default();
    siblings.sort();
    out.extend(siblings);
    out
}

/// The per-copy liveness lock inside a rendezvous dir.
const OWNER_LOCK: &str = "owner.lock";

/// Only the real rendezvous layout (`<cache>/bus/<pid>`) is claimed and
/// swept. A registry built over an arbitrary dir (`CapabilityRegistry::new`
/// uses the temp dir as its parent) must never sweep that parent: a
/// numeric-named dir there belongs to someone else.
fn is_bus_root(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| n == "bus")
}

/// Create this copy's rendezvous dir and take its exclusive owner lock.
/// `None` when the dir is not under a bus root or the lock is already held
/// (another copy in this process shares the dir) — the copy then simply
/// does not advertise liveness, exactly like a pre-lock binary.
fn claim_rendezvous(rendezvous: &Path) -> Option<std::fs::File> {
    if !rendezvous.parent().is_some_and(is_bus_root) {
        return None;
    }
    std::fs::create_dir_all(rendezvous).ok()?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(rendezvous.join(OWNER_LOCK))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Whether the copy that owns `dir` is gone. A dir with an owner lock is
/// dead exactly when the lock can be taken. A dir without one was written
/// by a binary that predates the lock (or is being created this instant):
/// it is dead when its pid is not a live susi process.
fn rendezvous_is_dead(dir: &Path, pid: u32, live: &mut LiveProcesses) -> bool {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(OWNER_LOCK))
    {
        Ok(file) => match file.try_lock() {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(_)) => false,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => !live.is_susi(pid),
        Err(_) => false,
    }
}

/// Live-process view for legacy rendezvous dirs, read at most once per
/// sweep (and only when such a dir exists).
#[derive(Default)]
struct LiveProcesses {
    #[cfg(windows)]
    images: Option<std::collections::HashMap<u32, String>>,
}

impl LiveProcesses {
    /// Linux keeps its `/proc` liveness rule for legacy dirs.
    #[cfg(target_os = "linux")]
    fn is_susi(&mut self, pid: u32) -> bool {
        PathBuf::from(format!("/proc/{pid}")).exists()
    }

    /// True when `pid` names a live susi process. A pid that was reused by
    /// an unrelated program (observed: dead daemons' dirs whose numbers now
    /// belong to `conhost`, `rustc`, `svchost`) does not count.
    #[cfg(windows)]
    fn is_susi(&mut self, pid: u32) -> bool {
        let images = self.images.get_or_insert_with(windows_process_images);
        if images.is_empty() {
            // The listing failed: keep every legacy dir rather than guess.
            return true;
        }
        images
            .get(&pid)
            .is_some_and(|image| image.to_ascii_lowercase().starts_with("susi"))
    }

    /// Other platforms have no cheap, safe liveness probe here: legacy dirs
    /// are kept (owner-locked dirs are still swept).
    #[cfg(not(any(target_os = "linux", windows)))]
    fn is_susi(&mut self, _pid: u32) -> bool {
        true
    }
}

/// `pid → image name` for every process, from one `tasklist` call.
#[cfg(windows)]
fn windows_process_images() -> std::collections::HashMap<u32, String> {
    let Ok(out) = susi_paths::console::background_command("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return std::collections::HashMap::new();
    };
    parse_tasklist_csv(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `tasklist /FO CSV /NH` rows (`"image","pid",…`).
#[cfg(any(windows, test))]
fn parse_tasklist_csv(text: &str) -> std::collections::HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"");
            let image = fields.next()?.trim().trim_start_matches('"').to_string();
            let pid = fields.next()?.trim().trim_end_matches('"').parse().ok()?;
            Some((pid, image))
        })
        .collect()
}

/// Whether `dir` looks like a rendezvous dir — what a sweep may remove.
fn looks_like_rendezvous(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut rd) => rd.all(|e| {
            e.ok().is_some_and(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                matches!(
                    name.as_ref(),
                    OWNER_LOCK | "topics" | "prefixes" | "caps" | "evidence" | "intents" | "broker"
                ) || is_rendezvous_manifest_name(&name)
                    || name.ends_with(".tmp")
            })
        }),
        Err(_) => false,
    }
}

/// Prefix of a swept dir awaiting deletion. Not numeric, so no lookup ever
/// scans it.
const SWEPT_PREFIX: &str = ".swept-";
/// Held by whichever process is sweeping a bus right now.
const SWEEP_LOCK: &str = ".sweep.lock";

/// Sweep the bus this copy just joined, off the caller's thread: a backlog
/// is slow to retire on Windows (measured: 727 dirs held a daemon's boot
/// for 68 s when swept inline), and nothing a caller does needs the sweep
/// finished — a stale dir only costs a lookup some I/O until it is gone.
fn spawn_sweep(own: &Path) {
    let own = own.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("bus-sweep".into())
        .spawn(move || {
            sweep_dead_processes(own.parent(), &own);
        });
}

/// Retire rendezvous dirs whose owning copy is gone. Every process sweeps
/// once when its bus starts, on every platform: a stale dir costs every
/// later lookup a scan of its files and a connect timeout per dead
/// endpoint, and on Windows — where nothing used to sweep — 727 of them
/// (22k files) had accumulated under one home.
///
/// Every dead dir is first renamed out of the lookup set, then deleted;
/// whatever a short-lived process leaves half-deleted, the next sweep
/// finishes. Returns `false` only when another sweeper holds the bus.
fn sweep_dead_processes(bus_dir: Option<&Path>, own: &Path) -> bool {
    let Some(dir) = bus_dir.filter(|d| is_bus_root(d)) else {
        return true;
    };
    // One sweeper per bus at a time: every process starts one, and on a
    // loaded Windows host the legacy-dir process listing alone took 4–99 s —
    // a burst of CLI commands must not run a burst of listings.
    let Ok(sweeper) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(SWEEP_LOCK))
    else {
        return true;
    };
    if sweeper.try_lock().is_err() {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return true;
    };
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut live = LiveProcesses::default();
    let mut trash = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path == own || !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(SWEPT_PREFIX) {
            trash.push(path);
            continue;
        }
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        if looks_like_rendezvous(&path) && rendezvous_is_dead(&path, pid, &mut live) {
            let swept = dir.join(format!(
                "{SWEPT_PREFIX}{name}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            if std::fs::rename(&path, &swept).is_ok() {
                trash.push(swept);
            }
        }
    }
    for dir in trash {
        let _ = std::fs::remove_dir_all(dir);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_slots_cap_and_release() {
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a = ConnSlot::claim(&live, 2).unwrap();
        let b = ConnSlot::claim(&live, 2).unwrap();
        assert!(ConnSlot::claim(&live, 2).is_none());
        drop(a);
        let c = ConnSlot::claim(&live, 2);
        assert!(c.is_some());
        drop((b, c));
        assert_eq!(live.load(std::sync::atomic::Ordering::Acquire), 0);
    }
    use std::sync::atomic::AtomicU64;

    struct Echo;
    impl PlaneHandler for Echo {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            Ok(json!({ "echo": payload }))
        }
    }

    struct Reject;
    impl PlaneHandler for Reject {
        fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
            Err("nope".to_string())
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "susi_bus_ipc_{}_{}_{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Raw HTTP/1.0 POST to `/handle`, optionally with a bus key.
    fn raw_handle(ep: SocketAddr, key: Option<&str>) -> String {
        let body = json!({ "topic": "test.echo", "payload": { "n": 7 } }).to_string();
        let key_line = key.map_or(String::new(), |k| format!("{BUS_KEY_HEADER}: {k}\r\n"));
        let req = format!(
            "POST /handle HTTP/1.0\r\nHost: 127.0.0.1\r\n{key_line}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut s = TcpStream::connect(ep).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn requests_without_the_bus_key_are_refused() {
        let dir = tempdir("auth");
        let a = IpcPlaneBus::with_rendezvous(dir.join("1"));
        a.register("test.echo", Arc::new(Echo));
        let ep = a.endpoint().unwrap();

        // A browser no-CORS POST or another local user: no / wrong key.
        for key in [None, Some("0".repeat(64).as_str())] {
            let resp = raw_handle(ep, key);
            assert!(resp.starts_with("HTTP/1.0 403"), "{resp}");
            assert!(!resp.contains("\"n\":7"), "handler must not run: {resp}");
        }
        // The owner's processes share the secret and are served.
        let resp = raw_handle(ep, Some(&a.secret));
        assert!(resp.starts_with("HTTP/1.0 200"), "{resp}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secret"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "bus secret must be owner-only");
        }
    }

    #[test]
    fn processes_sharing_a_bus_dir_share_one_secret() {
        let dir = tempdir("shared");
        let a = IpcPlaneBus::with_rendezvous(dir.join("1"));
        let b = IpcPlaneBus::with_rendezvous(dir.join("2"));
        assert_eq!(a.secret, b.secret);
        assert_eq!(a.secret.len(), 64);
    }

    #[test]
    fn cross_copy_request_reaches_registered_handler() {
        let dir = tempdir("req");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        a.register("test.echo", Arc::new(Echo));
        let out = b.request("test.echo", json!({ "n": 1 })).unwrap();
        assert_eq!(out, json!({ "echo": { "n": 1 } }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handler_error_propagates_to_caller() {
        let dir = tempdir("err");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        a.register("test.reject", Arc::new(Reject));
        assert_eq!(b.request("test.reject", json!({})).unwrap_err(), "nope");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefix_registration_matches_subtopics() {
        let dir = tempdir("prefix");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        a.register_prefix("gemi.", Arc::new(Echo));
        let out = b.request("gemi.infer.generate", json!({ "x": 2 })).unwrap();
        assert_eq!(out, json!({ "echo": { "x": 2 } }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_copy_request_stays_in_process() {
        let dir = tempdir("local");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        a.register("test.local", Arc::new(Echo));
        let out = a.request("test.local", json!({ "y": 3 })).unwrap();
        assert_eq!(out, json!({ "echo": { "y": 3 } }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stream_emit_reaches_requesters_receiver() {
        let dir = tempdir("stream");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        let (sid, rx) = b.open_stream();
        // Emitting from a different copy routes over loopback into b's flume.
        a.stream_emit(&sid, json!({ "text": "chunk-1" }));
        a.stream_emit(&sid, json!("chunk-2"));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            json!({ "text": "chunk-1" })
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            json!("chunk-2")
        );
        b.stream_close(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two rendezvous dirs under one `bus/` root simulate separate
    /// processes: the requester's scan must find a handler published by
    /// the sibling pid dir. A real `sleep` child provides a live pid so
    /// `sweep_dead_processes` does not reap its dir on Linux.
    #[test]
    fn cross_process_request_reaches_sibling_pid_dir() {
        let bus_root = tempdir("busroot");
        let mut child = match std::process::Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return, // no sleep binary — skip
        };
        let mine = bus_root.join(std::process::id().to_string());
        let theirs = bus_root.join(child.id().to_string());
        let a = IpcPlaneBus::with_rendezvous(mine);
        let b = IpcPlaneBus::with_rendezvous(theirs);
        b.register("test.cross", Arc::new(Echo));
        let out = a.request("test.cross", json!({ "p": 7 })).unwrap();
        assert_eq!(out, json!({ "echo": { "p": 7 } }));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&bus_root);
    }

    #[test]
    fn stale_endpoint_file_is_pruned() {
        let dir = tempdir("stale");
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        let tdir = dir.join("topics");
        std::fs::create_dir_all(&tdir).unwrap();
        let file = tdir.join(enc("test.dead"));
        std::fs::write(
            &file,
            json!({ "endpoint": "127.0.0.1:1", "key": "test.dead" }).to_string(),
        )
        .unwrap();
        assert!(b.request("test.dead", json!({})).is_err());
        assert!(!file.exists(), "dead endpoint file must be pruned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An endpoint file whose owner died while another copy took its port
    /// is answered "no handler" by that copy. The request must treat it as
    /// a dead candidate — prune it, try the next — rather than return the
    /// stranger's error (observed: 150–200 `no handler for topic
    /// capability.provider.…` failures a minute after a daemon boot).
    #[test]
    fn stale_endpoint_reused_by_a_live_copy_is_skipped_and_pruned() {
        let root = tempdir("reused");
        let squatter = IpcPlaneBus::with_rendezvous(root.join("1"));
        squatter.register("test.unrelated", Arc::new(Reject));
        let owner = IpcPlaneBus::with_rendezvous(root.join("2"));
        owner.register("test.echo", Arc::new(Echo));
        // A dead copy's file, sorted first, pointing at the squatter's port.
        let stale = root.join("0").join("topics");
        std::fs::create_dir_all(&stale).unwrap();
        let stale = stale.join(enc("test.echo"));
        std::fs::write(
            &stale,
            json!({ "endpoint": squatter.endpoint().unwrap().to_string(), "key": "test.echo" })
                .to_string(),
        )
        .unwrap();
        let caller = IpcPlaneBus::with_rendezvous(root.join("3"));
        let out = caller.request("test.echo", json!({ "k": 1 })).unwrap();
        assert_eq!(out, json!({ "echo": { "k": 1 } }));
        assert!(!stale.exists(), "the misrouted endpoint file is pruned");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A live owner whose handler relays a nested request's "no handler"
    /// text is answering, not missing: the caller gets that error, and the
    /// owner's endpoint file stays — pruning it would cut the topic off
    /// from every other process until the owner re-registered.
    #[test]
    fn a_handler_error_that_reads_like_a_miss_is_an_answer() {
        struct Relays;
        impl PlaneHandler for Relays {
            fn handle(&self, _topic: &str, _payload: Value) -> Result<Value, String> {
                Err(no_handler("test.unwired"))
            }
        }
        let root = tempdir("relayed-miss");
        let owner = IpcPlaneBus::with_rendezvous(root.join("1"));
        owner.register("test.relay", Arc::new(Relays));
        let file = owner.routes_file();
        assert!(file.exists());
        let caller = IpcPlaneBus::with_rendezvous(root.join("2"));
        let err = caller.request("test.relay", json!({})).unwrap_err();
        assert!(
            err.starts_with(NO_HANDLER),
            "the handler's own error: {err}"
        );
        assert!(err.contains("test.unwired"), "{err}");
        assert!(
            file.exists(),
            "a live owner's endpoint file is never pruned"
        );
        // And it keeps answering.
        assert!(caller.request("test.relay", json!({})).is_err());
        assert!(file.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn batched_registration_publishes_single_routes_manifest() {
        let root = bus_root("batched-routes");
        let own = root.join(std::process::id().to_string());
        let owner = IpcPlaneBus::with_rendezvous(own.clone());
        owner.begin_batch();
        for idx in 0..50 {
            owner.register(&format!("capability.tool.t{idx}"), Arc::new(Echo));
        }
        owner.register_prefix("gemi.", Arc::new(Echo));
        // Nothing touches disk until the batch closes.
        assert!(!owner.routes_file().exists());
        owner.end_batch();
        assert_eq!(owner.routes_file(), own.join("routes.json"));
        assert!(owner.routes_file().exists());
        assert!(!own.join("topics").exists());
        assert!(!own.join("prefixes").exists());

        let caller = IpcPlaneBus::with_rendezvous(root.join("999999"));
        assert_eq!(
            caller
                .request("capability.tool.t42", json!({ "v": 42 }))
                .unwrap(),
            json!({ "echo": { "v": 42 } })
        );
        assert_eq!(
            caller.request("gemi.chat", json!({ "v": 7 })).unwrap(),
            json!({ "echo": { "v": 7 } })
        );
        drop(caller);
        drop(owner);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// The serving side marks a dispatch miss structurally, beside the
    /// `err` text older callers read; a handler's error carries no marker.
    #[test]
    fn a_dispatch_miss_is_marked_structurally() {
        let handlers: HandlerMap = Arc::new(RwLock::new(HashMap::new()));
        let prefixes: HandlerMap = Arc::new(RwLock::new(HashMap::new()));
        handlers
            .write()
            .unwrap()
            .insert("test.reject".into(), Arc::new(Reject));
        assert!(dispatch(&handlers, &prefixes, "test.none", json!({})).is_none());
        assert_eq!(
            dispatch(&handlers, &prefixes, "test.reject", json!({})),
            Some(Err("nope".to_string()))
        );
        let root = tempdir("miss-marker");
        let copy = IpcPlaneBus::with_rendezvous(root.join("1"));
        copy.register("test.reject", Arc::new(Reject));
        let ep = copy.endpoint().unwrap();
        let ask = |topic: &str| -> Value {
            let body = json!({ "topic": topic, "payload": {} }).to_string();
            serde_json::from_str(&post(ep, "/handle", &body, &copy.secret).unwrap()).unwrap()
        };
        let miss = ask("test.none");
        assert_eq!(miss[MISS_FIELD], json!("test.none"));
        assert!(miss["err"].as_str().unwrap().starts_with(NO_HANDLER));
        let answered = ask("test.reject");
        assert!(answered.get(MISS_FIELD).is_none());
        assert_eq!(answered["err"], json!("nope"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn bus_root(tag: &str) -> PathBuf {
        let root = tempdir(tag).join("bus");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A copy joining the bus, with its sweep run to completion (in
    /// production it runs on a background thread).
    fn join_and_sweep(dir: PathBuf) -> IpcPlaneBus {
        let bus = IpcPlaneBus::with_rendezvous(dir.clone());
        // The copy's own background sweep may hold the sweep lock: wait for
        // a run of our own. That sweep lists processes when a legacy dir is
        // present, which took 4–99 s on a loaded Windows host; the old 20 s
        // budget then ran out and the caller asserted on a sweep still in
        // flight. Wait as long as such a listing can take, and fail loudly
        // rather than fall through to the assertions.
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        while !sweep_dead_processes(dir.parent(), &dir) {
            assert!(
                std::time::Instant::now() < deadline,
                "the bus sweep lock was still held after 300 s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        bus
    }

    /// A pid no live process can hold.
    const DEAD_PID: u32 = 4_000_000_007;

    /// A burst of processes joining one bus runs one sweep, not a burst.
    #[test]
    fn only_one_sweeper_runs_at_a_time() {
        let root = bus_root("sweep-one");
        let dead = root.join("301");
        std::fs::create_dir_all(dead.join("topics")).unwrap();
        std::fs::write(dead.join(OWNER_LOCK), b"").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(SWEEP_LOCK))
            .unwrap();
        held.try_lock().unwrap();
        let own = root.join("302");
        assert!(!sweep_dead_processes(Some(&root), &own), "busy: skipped");
        assert!(dead.exists());
        drop(held);
        assert!(sweep_dead_processes(Some(&root), &own));
        assert!(!dead.exists());
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    /// Joining a bus starts its sweep without waiting for it: the dead
    /// copy's dir disappears on its own shortly after.
    #[test]
    fn joining_the_bus_sweeps_dead_dirs_in_the_background() {
        let root = bus_root("sweep-bg");
        let dead = root.join("201");
        std::fs::create_dir_all(dead.join("prefixes")).unwrap();
        std::fs::write(dead.join(OWNER_LOCK), b"").unwrap();
        let _bus = IpcPlaneBus::with_rendezvous(root.join("202"));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while dead.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!dead.exists(), "the background sweep retired the dead dir");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn sweep_removes_owner_locked_dirs_whose_owner_is_gone_and_keeps_live_ones() {
        let root = bus_root("sweep-lock");
        // Dead: its owner lock is free (the process that held it exited).
        let dead = root.join("101");
        std::fs::create_dir_all(dead.join("topics")).unwrap();
        std::fs::write(dead.join(OWNER_LOCK), b"").unwrap();
        std::fs::write(dead.join("topics").join("x"), b"{}").unwrap();
        // Live: someone holds its owner lock right now.
        let live = root.join("102");
        let holder = claim_rendezvous(&live).expect("lock a fresh rendezvous");
        // A new copy starting on this bus sweeps once.
        let own = root.join("103");
        let _bus = join_and_sweep(own.clone());
        assert!(!dead.exists(), "a dir whose owner lock is free is swept");
        assert!(live.exists(), "a dir whose owner lock is held is kept");
        assert!(own.join(OWNER_LOCK).exists(), "the new copy owns its dir");
        // Its own lock is held for its lifetime: the next sweep keeps it.
        let _later = join_and_sweep(root.join("104"));
        assert!(own.exists());
        drop(holder);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn sweep_removes_legacy_dirs_of_processes_that_are_gone() {
        let root = bus_root("sweep-legacy");
        // A pre-lock binary's dir: no owner lock, and its pid is not alive.
        let legacy = root.join(DEAD_PID.to_string());
        std::fs::create_dir_all(legacy.join("caps").join("provider")).unwrap();
        std::fs::write(legacy.join("caps").join("provider").join("p"), b"{}").unwrap();
        let _bus = join_and_sweep(root.join("105"));
        assert!(!legacy.exists(), "a dead legacy dir is swept");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn sweep_never_touches_dirs_outside_a_bus_root_or_with_foreign_content() {
        // Not a bus root (a registry over the temp dir has such a parent).
        let other = tempdir("not-bus");
        let numeric = other.join(DEAD_PID.to_string());
        std::fs::create_dir_all(numeric.join("topics")).unwrap();
        let _a = join_and_sweep(other.join("106"));
        assert!(numeric.exists(), "only <cache>/bus/ is swept");
        assert!(
            !other.join("106").join(OWNER_LOCK).exists(),
            "and only a bus rendezvous is claimed"
        );
        // A bus root, but the numeric dir holds something a bus never writes.
        let root = bus_root("foreign");
        let foreign = root.join(DEAD_PID.to_string());
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("notes.txt"), b"keep me").unwrap();
        let _b = join_and_sweep(root.join("107"));
        assert!(foreign.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn tasklist_rows_parse_to_pid_and_image() {
        let rows = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\
                    \"susi.exe\",\"43644\",\"Console\",\"1\",\"812,100 K\"\r\n\
                    \"conhost.exe\",\"16904\",\"Console\",\"1\",\"7,000 K\"\r\n\
                    INFO: No tasks are running which match the specified criteria.\r\n";
        let map = parse_tasklist_csv(rows);
        assert_eq!(map.get(&43644).map(String::as_str), Some("susi.exe"));
        assert_eq!(map.get(&16904).map(String::as_str), Some("conhost.exe"));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn is_wired_sees_remote_registration() {
        let dir = tempdir("wired");
        let a = IpcPlaneBus::with_rendezvous(dir.clone());
        let b = IpcPlaneBus::with_rendezvous(dir.clone());
        assert!(!b.is_wired("test.wired"));
        a.register("test.wired", Arc::new(Echo));
        assert!(b.is_wired("test.wired"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A freshly minted bus secret grants only the current user, full
    /// control, with no inherited entries (the Windows twin of `0600`).
    #[cfg(windows)]
    #[test]
    fn a_new_bus_secret_grants_only_the_current_user() {
        let dir = tempdir("secret-acl");
        let secret = bus_secret(Some(&dir));
        assert_eq!(secret.len(), 64);
        let aces = winapi::acl::read_dacl(&dir.join("secret"))
            .unwrap()
            .unwrap();
        assert_eq!(
            aces,
            vec![winapi::acl::Ace {
                sid: winapi::acl::current_user_sid().unwrap(),
                deny: false,
                inherited: false,
                mask: winapi::acl::FILE_ALL_ACCESS,
            }]
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("secret")).unwrap(),
            secret,
            "the stored key is the returned key"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Failure injection: when the owner-only secret file cannot be created
    /// (here its directory cannot exist, because a file sits where a parent
    /// directory must be), the key is withheld (empty, matching no presented
    /// key) and nothing is left behind.
    #[cfg(windows)]
    #[test]
    fn a_bus_secret_that_cannot_be_stored_owner_only_is_withheld() {
        let dir = tempdir("secret-fail");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let bus = blocker.join("bus");
        assert_eq!(bus_secret(Some(&bus)), "");
        assert!(!key_matches("", ""), "an empty key matches nothing");
        assert_eq!(std::fs::read(&blocker).unwrap(), b"not a directory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A process that loses the exclusive create adopts the winner's key; it
    /// never rewrites or deletes the existing secret file.
    #[test]
    fn a_bus_secret_that_already_exists_is_adopted_unchanged() {
        let dir = tempdir("secret-existing");
        std::fs::create_dir_all(&dir).unwrap();
        let existing = "ab".repeat(32);
        std::fs::write(dir.join("secret"), &existing).unwrap();
        assert_eq!(bus_secret(Some(&dir)), existing);
        assert_eq!(
            std::fs::read_to_string(dir.join("secret")).unwrap(),
            existing
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
