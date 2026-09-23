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

    /// Explicit rendezvous dir — tests give each "process" its own dir tree
    /// without mutating process env.
    pub fn with_rendezvous(rendezvous: PathBuf) -> Self {
        sweep_dead_processes(rendezvous.parent());
        Self {
            handlers: Arc::new(DashMap::new()),
            prefixes: Arc::new(DashMap::new()),
            streams: Arc::new(DashMap::new()),
            stream_seq: AtomicU64::new(0),
            rendezvous,
            endpoint: OnceLock::new(),
        }
    }

    /// This copy's rendezvous dir — capability metadata and other
    /// process-scoped IPC state share the same `<cache>/bus/<pid>` tree.
    pub fn rendezvous(&self) -> &Path {
        &self.rendezvous
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
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let handlers = Arc::clone(&handlers);
                let prefixes = Arc::clone(&prefixes);
                let streams = Arc::clone(&streams);
                std::thread::spawn(move || serve_conn(conn, handlers, prefixes, streams));
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

    /// Ordered remote candidates: exact topic file first, then every prefix
    /// file whose key prefixes `topic`. Returns `(file, endpoint)` pairs so
    /// stale files can be pruned on connect failure.
    fn candidates(&self, topic: &str) -> Vec<(PathBuf, SocketAddr)> {
        let mut out = Vec::new();
        let exact = self.rendezvous.join("topics").join(enc(topic));
        if let Some((ep, _)) = read_endpoint_key(&exact) {
            out.push((exact, ep));
        }
        let pdir = self.rendezvous.join("prefixes");
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
    fn remote(ep: SocketAddr, topic: &str, payload: &Value) -> Remote {
        let Ok(body) = serde_json::to_string(&json!({
            "topic": topic,
            "payload": payload,
        })) else {
            return Remote::Answered(Err("bus request failed to serialize".to_string()));
        };
        let Some(resp) = post(ep, "/handle", &body) else {
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
            match Self::remote(ep, topic, &payload) {
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
                let _ = post(ep, "/stream", &body);
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

fn read_endpoint_key(path: &Path) -> Option<(SocketAddr, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let ep = v.get("endpoint")?.as_str()?.parse().ok()?;
    let key = v.get("key")?.as_str()?.to_string();
    Some((ep, key))
}

/// POST one JSON body over HTTP/1.0; returns the response body.
fn post(addr: SocketAddr, path: &str, body: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let req = format!(
        "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    buf.split("\r\n\r\n").nth(1).map(str::to_string)
}

/// Read one HTTP/1.0 request: headers, then exactly Content-Length bytes.
fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
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
    let mut body = buf[head_end..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_len);
    Some((path, String::from_utf8_lossy(&body).to_string()))
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

fn serve_conn(mut conn: TcpStream, handlers: HandlerMap, prefixes: HandlerMap, streams: StreamMap) {
    let Some((path, body)) = read_request(&mut conn) else {
        return;
    };
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

