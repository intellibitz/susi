//! IPC backend for `plane_bus` — the microkernel rendezvous.
//!
//! Vendored `susi_core` copies in feature crates cannot share the in-process
//! `PlaneBus::global()` static: each vendored module is a distinct type with
//! its own `OnceLock`. `IpcPlaneBus` gives every copy the same wire semantics
//! over loopback instead:
//!
//! - `register` / `register_prefix` lazily bind a per-copy `127.0.0.1:0`
//!   listener and publish the endpoint under `<cache>/bus/<pid>/` — a
//!   filesystem rendezvous scoped to the owning process (`std::process::id`),
//!   so parallel processes (daemon, CLI, test binaries) keep today's
//!   per-process bus isolation.
//! - `request` resolves a topic to its endpoint file and POSTs the JSON
//!   payload (`POST /handle` → `{ok|err}`); exact topic files are tried
//!   before prefix files, mirroring the in-process lookup order.
//! - `open_stream` embeds the caller's endpoint in the stream id
//!   (`ipc://<addr>/<id>`) so `stream_emit` reaches the right copy from
//!   anywhere; the `/stream` endpoint feeds the local flume receiver.
//! - Stale endpoint files are pruned on connect failure; dead pid dirs are
//!   swept on init where `/proc` is available.

use crate::susi_core::plane_bus::PlaneHandler;
use crate::susi_paths::SusiDirs;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Connect timeout: the rendezvous only ever targets this host.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Cap on request head bytes read while hunting for `\r\n\r\n`.
const MAX_HEAD: usize = 64 * 1024;
/// Cap on an IPC request body; larger declared lengths are refused unread.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// Header carrying the per-substrate bus secret on every IPC request.
const BUS_KEY_HEADER: &str = "X-Susi-Bus-Key";

type HandlerMap = Arc<DashMap<String, Arc<dyn PlaneHandler>>>;
type StreamMap = Arc<DashMap<String, flume::Sender<Value>>>;

/// Process-scoped IPC plane bus — same method surface as
/// [`crate::susi_core::plane_bus::PlaneBus`]. Vendored `susi_core` copies back their
/// `PlaneBus` with this so a handler registered through one vendored module
/// resolves from every other vendored copy in the same process.
pub struct IpcPlaneBus {
    handlers: HandlerMap,
    prefixes: HandlerMap,
    streams: StreamMap,
    stream_seq: AtomicU64,
    rendezvous: PathBuf,
    endpoint: OnceLock<SocketAddr>,
    /// Shared secret every `/handle` and `/stream` request must carry.
    secret: Arc<str>,
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

    /// The shared bus for this copy: all vendored modules in one consumer
    /// crate route through it, so a registration from `registry_ipc` is
    /// reachable via `plane_bus` lookups and vice versa.
    pub fn global() -> Arc<Self> {
        static BUS: OnceLock<Arc<IpcPlaneBus>> = OnceLock::new();
        Arc::clone(BUS.get_or_init(|| Arc::new(Self::new())))
    }

    /// Explicit rendezvous dir — tests give each "process" its own dir tree
    /// without mutating process env.
    pub fn with_rendezvous(rendezvous: PathBuf) -> Self {
        sweep_dead_processes(rendezvous.parent());
        let secret = bus_secret(rendezvous.parent()).into();
        Self {
            handlers: Arc::new(DashMap::new()),
            prefixes: Arc::new(DashMap::new()),
            streams: Arc::new(DashMap::new()),
            stream_seq: AtomicU64::new(0),
            rendezvous,
            endpoint: OnceLock::new(),
            secret,
        }
    }

    /// This copy's rendezvous dir — capability metadata and other
    /// process-scoped IPC state share the same `<cache>/bus/<pid>` tree.
    pub fn rendezvous(&self) -> &Path {
        &self.rendezvous
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
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let handlers = Arc::clone(&handlers);
                let prefixes = Arc::clone(&prefixes);
                let streams = Arc::clone(&streams);
                let secret = Arc::clone(&secret);
                std::thread::spawn(move || serve_conn(conn, handlers, prefixes, streams, &secret));
            }
        });
    }

    /// Publish `key`'s endpoint file under `rendezvous/<kind>/`.
    fn publish_endpoint(&self, kind: &str, key: &str) {
        let Some(ep) = self.endpoint() else { return };
        let dir = self.rendezvous.join(kind);
        let _ = std::fs::create_dir_all(&dir);
        let body = json!({ "endpoint": ep.to_string(), "key": key }).to_string();
        let tmp = dir.join(format!(".{}.tmp", enc(key)));
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(enc(key)));
        }
    }

    /// Ordered remote candidates: every exact topic file across live
    /// rendezvous dirs first, then every prefix file whose key prefixes
    /// `topic` — exact registrations outrank prefix handlers process-wide.
    /// Returns `(file, endpoint)` pairs so stale files can be pruned on
    /// connect failure.
    fn candidates(&self, topic: &str) -> Vec<(PathBuf, SocketAddr)> {
        let mut out = Vec::new();
        let dirs = self.rendezvous_dirs();
        for dir in &dirs {
            let exact = dir.join("topics").join(enc(topic));
            if let Some((ep, _)) = read_endpoint_key(&exact) {
                out.push((exact, ep));
            }
        }
        for dir in &dirs {
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
            .iter()
            .find(|e| topic.starts_with(e.key().as_str()))
            .map(|e| Arc::clone(e.value()))
    }

    /// `POST /handle` to a remote copy. `Remote::Dead` = transport failure
    /// (caller prunes the endpoint file and tries the next candidate).
    fn remote(ep: SocketAddr, topic: &str, payload: &Value, secret: &str) -> Remote {
        let Ok(body) = serde_json::to_string(&json!({
            "topic": topic,
            "payload": payload,
        })) else {
            return Remote::Answered(Err("bus request failed to serialize".to_string()));
        };
        let Some(resp) = post(ep, "/handle", &body, secret) else {
            return Remote::Dead;
        };
        let v: Value = match serde_json::from_str(&resp) {
            Ok(v) => v,
            Err(e) => {
                return Remote::Answered(Err(format!("malformed bus response: {e}")));
            }
        };
        if let Some(e) = v.get("err").and_then(|e| e.as_str()) {
            Remote::Answered(Err(e.to_string()))
        } else if let Some(ok) = v.get("ok") {
            Remote::Answered(Ok(ok.clone()))
        } else {
            Remote::Answered(Err("malformed bus response".to_string()))
        }
    }

    pub fn register(&self, topic: &str, handler: Arc<dyn PlaneHandler>) {
        self.handlers.insert(topic.to_string(), handler);
        self.publish_endpoint("topics", topic);
    }

    pub fn register_prefix(&self, prefix: &str, handler: Arc<dyn PlaneHandler>) {
        self.prefixes.insert(prefix.to_string(), handler);
        self.publish_endpoint("prefixes", prefix);
    }

    /// Remove an exact-topic registration made by this copy. The endpoint
    /// file is only deleted when it points at this copy's listener — another
    /// copy's registration is left alone.
    pub fn unregister(&self, topic: &str) -> bool {
        let removed = self.handlers.remove(topic).is_some();
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
        if let Some(h) = self.handlers.get(topic).map(|e| Arc::clone(e.value())) {
            return invoke(h, topic, payload);
        }
        for (file, ep) in self.candidates(topic) {
            match Self::remote(ep, topic, &payload, &self.secret) {
                Remote::Answered(res) => return res,
                Remote::Dead => {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        if let Some(h) = self.local_prefix(topic) {
            return invoke(h, topic, payload);
        }
        Err(format!(
            "plane bus: no handler for topic '{topic}' (composition root must register planes)"
        ))
    }

    pub fn publish(&self, topic: &str, payload: Value) {
        let _ = self.request(topic, payload);
    }

    /// Allocate a stream channel; the id embeds this copy's endpoint so
    /// `stream_emit` from any vendored copy reaches `rx` over loopback.
    pub fn open_stream(&self) -> (String, flume::Receiver<Value>) {
        let id = format!(
            "{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            self.stream_seq.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = flume::unbounded();
        self.streams.insert(id.clone(), tx);
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
                if let Some(tx) = self.streams.get(id) {
                    let _ = tx.send(chunk);
                }
            } else if let Ok(ep) = addr.parse::<SocketAddr>() {
                let body = json!({ "id": id, "chunk": chunk }).to_string();
                let _ = post(ep, "/stream", &body, &self.secret);
            }
        } else if let Some(tx) = self.streams.get(stream_id) {
            let _ = tx.send(chunk);
        }
    }

    pub fn stream_close(&self, stream_id: &str) {
        let id = stream_id
            .strip_prefix("ipc://")
            .and_then(|r| r.split_once('/').map(|(_, id)| id))
            .unwrap_or(stream_id);
        self.streams.remove(id);
    }

    pub fn is_wired(&self, topic: &str) -> bool {
        self.handlers.contains_key(topic)
            || self.local_prefix(topic).is_some()
            || !self.candidates(topic).is_empty()
    }
}

enum Remote {
    Answered(Result<Value, String>),
    Dead,
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
/// processes. Without a bus dir the secret is process-local.
fn bus_secret(bus_dir: Option<&Path>) -> String {
    let fresh = || {
        let mut raw = [0u8; 32];
        let _ = getrandom::fill(&mut raw);
        raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let Some(dir) = bus_dir else {
        return fresh();
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
    let candidate = fresh();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        if f.write_all(candidate.as_bytes()).is_ok() {
            return candidate;
        }
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

fn read_endpoint_key(path: &Path) -> Option<(SocketAddr, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let ep = v.get("endpoint")?.as_str()?.parse().ok()?;
    let key = v.get("key")?.as_str()?.to_string();
    Some((ep, key))
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
            let parsed = serde_json::from_str::<Value>(&body).ok();
            let result = parsed.map(|v| {
                let topic = v
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let payload = v.get("payload").cloned().unwrap_or(Value::Null);
                dispatch(&handlers, &prefixes, &topic, payload)
            });
            let resp = match result {
                Some(Ok(v)) => json!({ "ok": v }),
                Some(Err(e)) => json!({ "err": e }),
                None => json!({ "err": "malformed bus request" }),
            };
            respond(&mut conn, "200 OK", &resp.to_string());
        }
        "/stream" => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if let Some(tx) = streams.get(id) {
                    let _ = tx.send(v.get("chunk").cloned().unwrap_or(Value::Null));
                }
            }
            respond(&mut conn, "200 OK", "{}");
        }
        _ => respond(&mut conn, "404 Not Found", "{}"),
    }
}

/// Serving-side dispatch: exact topic match first, then prefix fallback.
fn dispatch(
    handlers: &HandlerMap,
    prefixes: &HandlerMap,
    topic: &str,
    payload: Value,
) -> Result<Value, String> {
    if let Some(h) = handlers.get(topic).map(|e| Arc::clone(e.value())) {
        return invoke(h, topic, payload);
    }
    let h = prefixes
        .iter()
        .find(|e| topic.starts_with(e.key().as_str()))
        .map(|e| Arc::clone(e.value()));
    match h {
        Some(h) => invoke(h, topic, payload),
        None => Err(format!(
            "plane bus: no handler for topic '{topic}' (composition root must register planes)"
        )),
    }
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

/// Remove rendezvous dirs whose pid no longer exists (`/proc` liveness —
/// Linux-only; other platforms keep stale dirs until manual cleanup).
fn sweep_dead_processes(bus_dir: Option<&Path>) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(dir) = bus_dir else { return };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.parse::<u32>().is_err() {
            continue;
        }
        if !PathBuf::from(format!("/proc/{name}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
