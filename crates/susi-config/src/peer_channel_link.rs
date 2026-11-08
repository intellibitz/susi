//! Sealed peer channel, transport (`susi-peer-channel-v1`): TCP framing,
//! multiplexed streams, the hub relay, outbound uplinks and the client API.
//!
//! Two layers, one code path (`docs/peer-channel.md`):
//!
//! - A **link** is one TCP connection between two nodes, sealed by a
//!   [`crate::peer_channel`] handshake under the link prologue. A member
//!   behind NAT dials its hub and keeps that link open (keepalive, reconnect)
//!   so the hub can call it; a caller with nothing open dials a short-lived
//!   link. Inside a link, frames open, carry and close numbered streams.
//! - Every call is an **end-to-end session** between the caller and the
//!   destination node, run as a second handshake (end-to-end prologue)
//!   inside one stream. A hub that forwards a stream from one member to
//!   another sees only that stream's frame headers (stream number,
//!   destination and source node ids) and ciphertext lengths: it holds
//!   neither end's session keys.
//!
//! On the wire a link is `MAGIC`, then messages of `u16 BE length | bytes`:
//! three handshake messages, then link transport messages. Nothing else is
//! ever written to the socket.
//!
//! Link frames (plaintext only inside a link transport message):
//! `kind u8 | stream u32 BE | body`, where `kind` is OPEN (body:
//! `ttl u8 | dst_len u8 | dst | src_len u8 | src`), DATA (one end-to-end
//! message), CLOSE (a reason), PING / PONG (keepalive), or HINT (the
//! sender's signed public address list).

use crate::peer_channel::{
    self as pc, ChannelError, CipherState, Expect, LocalIdentity, PeerIdentity,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// First bytes a dialer writes: protocol and version.
pub const MAGIC: &[u8; 8] = b"SUSIPC1\n";

/// The channel port a hub listens on when `peer_channel_listen` names an
/// address without a port. Outside the contract block (9090-9095 and its
/// offsets up to +2000), the leaf services (18080-18084) and the common
/// ephemeral ranges; never shifted by `port_offset`.
pub const DEFAULT_PORT: u16 = 29_090;

/// Link flag (encrypted in the first handshake payload): the dialer keeps
/// this link open to receive calls on it.
pub const FLAG_PERSISTENT: u8 = 0x01;

/// Keepalive cadence: a PING after this much read silence.
pub const KEEPALIVE: Duration = Duration::from_secs(15);

/// A link with no frame received for this long is dead.
pub const DEAD_AFTER: Duration = Duration::from_secs(45);

/// A short-lived link with no open stream closes after this much idleness.
const TRANSIENT_IDLE: Duration = Duration::from_secs(30);

/// Bound on connect plus the three handshake messages.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on one blocked socket write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Hops a stream may still take; each relay decrements it.
const MAX_TTL: u8 = 4;

/// Concurrent streams one link carries.
const MAX_STREAMS: usize = 256;

/// Concurrent inbound handshakes a listener runs.
const MAX_HANDSHAKES: usize = 64;

/// Application bytes per end-to-end message (below the Noise limit after
/// the stream frame header and both AEAD tags).
const APP_CHUNK: usize = 60_000;

/// End-to-end messages a stream buffers before the reader drains them.
const STREAM_QUEUE: usize = 512;

const OPEN: u8 = 1;
const DATA: u8 = 2;
const CLOSE: u8 = 3;
const PING: u8 = 4;
const PONG: u8 = 5;
const HINT: u8 = 6;

/// The file in the config directory listing the hubs this node keeps an
/// uplink to: rows of `{node_id, pubkey, address, hints?, hint_ts?}`.
pub const HUBS_FILE: &str = "peer_channel_hubs.json";

/// Standing of an identity in this node's roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerClass {
    /// This node's own key (a local client of its own daemon).
    SelfNode,
    /// A roster member whose node key is bound to this node id.
    Member,
    /// Proved the cluster key, not (yet) bound in this roster: the same
    /// standing a cluster peer-bearer caller has. It may call this node; a
    /// hub never relays its streams to other members.
    Applicant,
}

/// The identity on the other end of a terminated end-to-end session, as the
/// served application sees it.
#[derive(Debug, Clone)]
pub struct ChannelPeer {
    pub identity: PeerIdentity,
    pub class: PeerClass,
    /// The TCP source of the link the session arrived on (a hub, or the
    /// caller's NAT router): routing information, never an identity.
    pub link_source: SocketAddr,
}

/// Classify a proven identity against `peers.json` / `peers_banned.json`.
/// Banned identities and identities that claim a bound member's node id
/// under another key (or a bound key under another node id) are refused.
pub fn classify(peer: &PeerIdentity) -> Result<PeerClass, String> {
    if crate::cluster_key::node_pubkey_hex()
        .is_some_and(|own| own.eq_ignore_ascii_case(&peer.pubkey))
    {
        return Ok(PeerClass::SelfNode);
    }
    let banned = crate::cluster_key::member_banned(|b| {
        b.get("node_id").and_then(|v| v.as_str()) == Some(peer.node_id.as_str())
            || b.get("pubkey")
                .and_then(|v| v.as_str())
                .is_some_and(|k| !k.is_empty() && k.eq_ignore_ascii_case(&peer.pubkey))
    });
    if banned {
        return Err(format!("{} is banned from this cluster", peer.node_id));
    }
    let roster = crate::cluster_key::config_json_rows("peers.json");
    let mut class = PeerClass::Applicant;
    for row in &roster {
        let id = row.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let key = row.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let same_key = key.eq_ignore_ascii_case(&peer.pubkey);
        if id == peer.node_id && same_key {
            class = PeerClass::Member;
        } else if id == peer.node_id {
            return Err(format!(
                "{} is bound to a different node key in this roster",
                peer.node_id
            ));
        } else if same_key {
            return Err(format!(
                "node key of {} is bound to node {id} in this roster",
                peer.node_id
            ));
        }
    }
    Ok(class)
}

// ---- framing ----

fn write_message(socket: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    let len = u16::try_from(bytes.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "channel message too long")
    })?;
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    socket.write_all(&out)
}

fn read_message(socket: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    socket.read_exact(&mut len)?;
    let mut buf = vec![0u8; usize::from(u16::from_be_bytes(len))];
    socket.read_exact(&mut buf)?;
    Ok(buf)
}

fn frame(kind: u8, sid: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(kind);
    out.extend_from_slice(&sid.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn open_body(ttl: u8, dst: &str, src: &str) -> Vec<u8> {
    let mut body = vec![ttl];
    for id in [dst, src] {
        body.push(u8::try_from(id.len()).unwrap_or(0));
        body.extend_from_slice(id.as_bytes());
    }
    body
}

/// `(ttl, dst, src)` of an OPEN body.
fn parse_open(body: &[u8]) -> Option<(u8, String, String)> {
    let (&ttl, rest) = body.split_first()?;
    let (&dl, rest) = rest.split_first()?;
    let dl = usize::from(dl);
    if rest.len() < dl {
        return None;
    }
    let (dst, rest) = rest.split_at(dl);
    let (&sl, rest) = rest.split_first()?;
    if rest.len() != usize::from(sl) {
        return None;
    }
    Some((
        ttl,
        String::from_utf8(dst.to_vec()).ok()?,
        String::from_utf8(rest.to_vec()).ok()?,
    ))
}

// ---- links ----

enum Event {
    Data(Vec<u8>),
    Closed(String),
}

#[derive(Clone)]
enum Slot {
    Local(mpsc::SyncSender<Event>),
    Forward { link: Arc<Link>, sid: u32 },
}

/// One sealed TCP connection to another node.
pub struct Link {
    id: u64,
    peer: PeerIdentity,
    remote: SocketAddr,
    /// This side dialed the connection.
    dialed: bool,
    /// The peer may label streams with other nodes' ids (we dialed it as a
    /// hub). On a link we accepted, the source is always the link's peer.
    relay_trusted: bool,
    /// Kept open to receive calls on (dialer's flag).
    persistent: bool,
    writer: Mutex<(TcpStream, CipherState)>,
    streams: Mutex<HashMap<u32, Slot>>,
    next_sid: AtomicU32,
    last_rx: Mutex<Instant>,
    last_use: Mutex<Instant>,
    closed: AtomicBool,
}

impl Link {
    /// The identity this link's handshake proved.
    pub fn peer(&self) -> &PeerIdentity {
        &self.peer
    }

    /// The TCP address of the other end.
    pub fn remote(&self) -> SocketAddr {
        self.remote
    }

    /// Whether this side dialed the link and keeps it open to be called on.
    pub fn is_uplink(&self) -> bool {
        self.dialed && self.persistent
    }

    /// Whether the link is still usable.
    pub fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    fn touch(&self) {
        *self.last_use.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn send(&self, kind: u8, sid: u32, body: &[u8]) -> Result<(), String> {
        if !self.is_open() {
            return Err(format!("link to {} is closed", self.peer.node_id));
        }
        let plain = frame(kind, sid, body);
        let mut guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let (socket, cipher) = &mut *guard;
        let sealed = cipher.encrypt(&[], &plain).map_err(|e| e.to_string())?;
        if let Err(e) = write_message(socket, &sealed) {
            drop(guard);
            self.close(&format!("write failed: {e}"));
            return Err(format!("link to {}: {e}", self.peer.node_id));
        }
        Ok(())
    }

    /// Stream numbers: the dialer opens odd ones, the acceptor even ones.
    fn alloc_sid(&self) -> u32 {
        let n = self.next_sid.fetch_add(1, Ordering::SeqCst);
        let base = n.wrapping_mul(2);
        if self.dialed { base | 1 } else { base & !1 }.max(2)
    }

    fn sid_is_peers(&self, sid: u32) -> bool {
        // The peer opens odd streams when it dialed us, even when we dialed.
        (sid & 1 == 1) != self.dialed
    }

    fn slot(&self, sid: u32) -> Option<Slot> {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&sid)
            .cloned()
    }

    fn take_slot(&self, sid: u32) -> Option<Slot> {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&sid)
    }

    fn insert_slot(&self, sid: u32, slot: Slot) -> bool {
        let mut streams = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        if streams.len() >= MAX_STREAMS || streams.contains_key(&sid) {
            return false;
        }
        streams.insert(sid, slot);
        true
    }

    /// Close the link: every local stream sees end-of-stream, every
    /// forwarded stream is closed on its other link.
    pub fn close(&self, reason: &str) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(guard) = self.writer.lock() {
            let _ = guard.0.shutdown(std::net::Shutdown::Both);
        }
        let slots: Vec<Slot> = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, s)| s)
            .collect();
        for slot in slots {
            match slot {
                Slot::Local(tx) => {
                    let _ = tx.try_send(Event::Closed(reason.to_string()));
                }
                Slot::Forward { link, sid } => {
                    link.take_slot(sid);
                    let _ = link.send(CLOSE, sid, b"relay link closed");
                }
            }
        }
        registry().remove(self.id);
    }

    /// Open a stream to `dst` on this link and return its endpoint.
    fn open_stream(self: &Arc<Self>, dst: &str) -> Result<StreamEnd, String> {
        let sid = self.alloc_sid();
        let (tx, rx) = mpsc::sync_channel(STREAM_QUEUE);
        if !self.insert_slot(sid, Slot::Local(tx)) {
            return Err(format!("link to {} has no free stream", self.peer.node_id));
        }
        let own = crate::cluster_key::wire_node_id();
        self.touch();
        if let Err(e) = self.send(OPEN, sid, &open_body(MAX_TTL, dst, &own)) {
            self.take_slot(sid);
            return Err(e);
        }
        Ok(StreamEnd::new(Arc::clone(self), sid, rx))
    }

    /// The reader loop: decrypts every frame, dispatches it, keeps the link
    /// alive and ends it on silence, tampering or idleness.
    fn run_reader(self: Arc<Self>, mut socket: TcpStream, mut cipher: CipherState) {
        let _ = socket.set_read_timeout(Some(KEEPALIVE));
        let reason = loop {
            match read_message(&mut socket) {
                Ok(sealed) => {
                    *self.last_rx.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                    let plain = match cipher.decrypt(&[], &sealed) {
                        Ok(p) => p,
                        Err(e) => break format!("link message refused: {e}"),
                    };
                    if let Err(e) = self.dispatch(&plain) {
                        break e;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let silent = self
                        .last_rx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .elapsed();
                    if silent >= DEAD_AFTER {
                        break "keepalive: peer silent".to_string();
                    }
                    let idle = self
                        .last_use
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .elapsed();
                    let empty = self
                        .streams
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_empty();
                    if !self.persistent && empty && idle >= TRANSIENT_IDLE {
                        break "idle".to_string();
                    }
                    if self.send(PING, 0, b"").is_err() {
                        break "keepalive write failed".to_string();
                    }
                }
                Err(e) => break format!("link read: {e}"),
            }
            if !self.is_open() {
                break "closed".to_string();
            }
        };
        self.close(&reason);
    }

    fn dispatch(self: &Arc<Self>, plain: &[u8]) -> Result<(), String> {
        if plain.len() < 5 {
            return Err("short link frame".to_string());
        }
        let kind = plain[0];
        let sid = u32::from_be_bytes([plain[1], plain[2], plain[3], plain[4]]);
        let body = &plain[5..];
        match kind {
            OPEN => self.on_open(sid, body),
            DATA => {
                self.touch();
                match self.slot(sid) {
                    Some(Slot::Local(tx)) => {
                        if tx.try_send(Event::Data(body.to_vec())).is_err() {
                            self.take_slot(sid);
                            let _ = self.send(CLOSE, sid, b"stream overrun");
                        }
                    }
                    Some(Slot::Forward { link, sid: out }) => {
                        relay_trace(body);
                        if link.send(DATA, out, body).is_err() {
                            self.take_slot(sid);
                            let _ = self.send(CLOSE, sid, b"relay target gone");
                        }
                    }
                    None => {}
                }
                Ok(())
            }
            CLOSE => {
                match self.take_slot(sid) {
                    Some(Slot::Local(tx)) => {
                        let _ =
                            tx.try_send(Event::Closed(String::from_utf8_lossy(body).into_owned()));
                    }
                    Some(Slot::Forward { link, sid: out }) => {
                        link.take_slot(out);
                        let _ = link.send(CLOSE, out, body);
                    }
                    None => {}
                }
                Ok(())
            }
            PING => self.send(PONG, sid, body),
            PONG => Ok(()),
            HINT => {
                self.on_hint(body);
                Ok(())
            }
            _ => Err(format!("unknown link frame kind {kind}")),
        }
    }

    fn on_open(self: &Arc<Self>, sid: u32, body: &[u8]) -> Result<(), String> {
        let Some((ttl, dst, src)) = parse_open(body) else {
            return Err("malformed OPEN".to_string());
        };
        if !self.sid_is_peers(sid) {
            return Err("OPEN on a stream number this side owns".to_string());
        }
        self.touch();
        // On a link we accepted, the stream's source is the link's peer,
        // whatever the frame says.
        let src = if self.relay_trusted {
            src
        } else {
            self.peer.node_id.clone()
        };
        let own = crate::cluster_key::wire_node_id();
        if dst == own {
            let (tx, rx) = mpsc::sync_channel(STREAM_QUEUE);
            if !self.insert_slot(sid, Slot::Local(tx)) {
                return self.send(CLOSE, sid, b"too many streams");
            }
            let end = StreamEnd::new(Arc::clone(self), sid, rx);
            let link = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("susi-channel-serve".into())
                .spawn(move || serve_incoming(link, end, src));
            if spawned.is_err() {
                self.take_slot(sid);
                return self.send(CLOSE, sid, b"no thread");
            }
            return Ok(());
        }
        // Relay. Only this node's own clients and bound members may route
        // through it; an applicant may only call this node.
        let origin = match classify(&self.peer) {
            Ok(c @ (PeerClass::SelfNode | PeerClass::Member)) => c,
            Ok(PeerClass::Applicant) => {
                return self.send(CLOSE, sid, b"relay refused: not a bound member");
            }
            Err(e) => return self.send(CLOSE, sid, e.as_bytes()),
        };
        if ttl <= 1 {
            return self.send(CLOSE, sid, b"relay refused: ttl exhausted");
        }
        let Some(out) = registry().relay_target(&dst, self, origin) else {
            return self.send(CLOSE, sid, format!("no route to {dst}").as_bytes());
        };
        let out_sid = out.alloc_sid();
        if !self.insert_slot(
            sid,
            Slot::Forward {
                link: Arc::clone(&out),
                sid: out_sid,
            },
        ) {
            return self.send(CLOSE, sid, b"too many streams");
        }
        if !out.insert_slot(
            out_sid,
            Slot::Forward {
                link: Arc::clone(self),
                sid,
            },
        ) {
            self.take_slot(sid);
            return self.send(CLOSE, sid, b"relay target busy");
        }
        out.touch();
        if out
            .send(OPEN, out_sid, &open_body(ttl - 1, &dst, &src))
            .is_err()
        {
            out.take_slot(out_sid);
            self.take_slot(sid);
            return self.send(CLOSE, sid, b"relay target gone");
        }
        Ok(())
    }

    /// A signed address list from a hub this node dialed: stored as the
    /// hub's dial hints when it verifies under the hub's key and is newer
    /// than the stored one. The link already authenticated the sender; the
    /// signature makes the stored hint verifiable on its own.
    fn on_hint(&self, body: &[u8]) {
        if !self.dialed {
            return;
        }
        let Some(hint) = std::str::from_utf8(body)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        else {
            return;
        };
        let ts = hint.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        let addrs: Vec<String> = hint
            .get("addresses")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .take(8)
                    .collect()
            })
            .unwrap_or_default();
        let sig = hint.get("sig").and_then(|v| v.as_str()).unwrap_or("");
        if !crate::cluster_key::member_verify(
            &self.peer.pubkey,
            &hint_payload(&self.peer.node_id, ts, &addrs),
            sig,
        ) {
            return;
        }
        let _ = update_hub_hints(&self.peer, ts, &addrs, sig);
    }
}

/// Diagnostic: when `SUSI_CHANNEL_RELAY_TRACE` names a file at process
/// start, every stream body this node forwards for others is appended to it
/// (`u32 BE length | bytes`), exactly as the relay sees it after opening its
/// own link encryption. It shows what a hub can read of the calls it relays:
/// end-to-end ciphertext only.
fn relay_trace(body: &[u8]) {
    static TRACE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let trace = TRACE.get_or_init(|| {
        let path = std::env::var_os("SUSI_CHANNEL_RELAY_TRACE")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });
    if let Some(file) = trace {
        let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
        let len = u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes();
        let _ = file.write_all(&len).and_then(|()| file.write_all(body));
    }
}

/// Close every link this process holds (after this process's identity or
/// posture changed, or before it exits).
pub fn close_links() {
    for link in registry().snapshot() {
        link.close("closed by this process");
    }
}

/// The string a node signs to publish its own dial addresses.
pub fn hint_payload(node_id: &str, ts: u64, addrs: &[String]) -> String {
    format!(
        "susi-peer-channel-v1/hint:{node_id}:{ts}:{}",
        addrs.join(",")
    )
}

/// This node's signed hint frame body for `addrs`.
fn signed_hint(addrs: &[String]) -> Option<Vec<u8>> {
    let node = crate::cluster_key::wire_node_id();
    let ts = unix_now();
    let sig = crate::cluster_key::member_sign(&hint_payload(&node, ts, addrs))?;
    serde_json::to_vec(&serde_json::json!({"ts": ts, "addresses": addrs, "sig": sig})).ok()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- registry ----

type ServeHook = Box<dyn Fn(ChannelStream, ChannelPeer) + Send + Sync>;

struct Registry {
    links: Mutex<Vec<Arc<Link>>>,
    serve: OnceLock<ServeHook>,
    next_id: AtomicU64,
    link_replay: Mutex<pc::ReplayCache>,
    e2e_replay: Mutex<pc::ReplayCache>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        links: Mutex::new(Vec::new()),
        serve: OnceLock::new(),
        next_id: AtomicU64::new(1),
        link_replay: Mutex::new(pc::new_replay_cache()),
        e2e_replay: Mutex::new(pc::new_replay_cache()),
    })
}

impl Registry {
    fn add(&self, link: Arc<Link>) {
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(link);
    }

    fn remove(&self, id: u64) {
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|l| l.id != id);
    }

    fn snapshot(&self) -> Vec<Arc<Link>> {
        self.links
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|l| l.is_open())
            .cloned()
            .collect()
    }

    /// Where a relayed stream for `dst` goes: the persistent link of `dst`
    /// itself; failing that, for a stream that came up from a downstream
    /// link, one of this node's own uplinks.
    fn relay_target(&self, dst: &str, from: &Arc<Link>, origin: PeerClass) -> Option<Arc<Link>> {
        let links = self.snapshot();
        // Newest first: a member that reconnected is reached on its new
        // link even before the old one's keepalive declares it dead.
        let direct = links.iter().rev().find(|l| {
            l.id != from.id
                && l.persistent
                && l.peer.node_id == dst
                && match classify(&l.peer) {
                    Ok(PeerClass::Member | PeerClass::SelfNode) => true,
                    Ok(PeerClass::Applicant) => origin == PeerClass::SelfNode,
                    Err(_) => false,
                }
        });
        if let Some(l) = direct {
            return Some(Arc::clone(l));
        }
        if from.relay_trusted {
            return None;
        }
        links
            .iter()
            .find(|l| l.id != from.id && l.is_uplink() && l.peer.node_id != from.peer.node_id)
            .cloned()
    }
}

/// Install the handler for end-to-end sessions that terminate in this
/// process (the daemon serves them to its GMCP surface). Returns false
/// when one is already installed. A process without one refuses incoming
/// streams.
pub fn install_service(hook: impl Fn(ChannelStream, ChannelPeer) + Send + Sync + 'static) -> bool {
    registry().serve.set(Box::new(hook)).is_ok()
}

/// The open links of this process, for status reports.
pub fn links() -> Vec<Arc<Link>> {
    registry().snapshot()
}

// ---- handshakes ----

fn local_identity() -> Result<LocalIdentity, String> {
    LocalIdentity::load().ok_or_else(|| ChannelError::NoLocalIdentity.to_string())
}

fn first_psk() -> Result<[u8; 32], String> {
    pc::cluster_psks()
        .into_iter()
        .next()
        .ok_or_else(|| "no cluster key at cluster.key: the peer channel needs one".to_string())
}

/// Policy a dial consults for each address it would connect to; an `Err`
/// skips that address with the reason.
pub type DialPolicy<'a> = &'a (dyn Fn(&SocketAddr) -> Result<(), String> + Send + Sync);

/// How a dialed link will be used.
#[derive(Debug, Clone, Copy)]
pub struct DialMode {
    /// Keep the link open to receive calls on (a member's uplink).
    pub persistent: bool,
    /// The far end is a hub: streams arriving from it may name other nodes
    /// as their source.
    pub hub: bool,
}

/// Dial `addr` (`host:port`; a name is resolved on every dial), run the link
/// handshake, and register the link.
pub fn dial(
    addr: &str,
    expect: Expect,
    mode: DialMode,
    policy: DialPolicy<'_>,
) -> Result<Arc<Link>, String> {
    let targets: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {addr}: {e}"))?
        .collect();
    let mut errors = Vec::new();
    for target in targets {
        if let Err(e) = policy(&target) {
            errors.push(format!("{target}: {e}"));
            continue;
        }
        match dial_one(target, expect.clone(), mode) {
            Ok(link) => return Ok(link),
            Err(e) => errors.push(format!("{target}: {e}")),
        }
    }
    if errors.is_empty() {
        errors.push("no address".to_string());
    }
    Err(format!("peer channel to {addr}: {}", errors.join("; ")))
}

fn dial_one(target: SocketAddr, expect: Expect, mode: DialMode) -> Result<Arc<Link>, String> {
    let local = local_identity()?;
    let psk = first_psk()?;
    let mut socket = TcpStream::connect_timeout(&target, HANDSHAKE_TIMEOUT)
        .map_err(|e| format!("connect: {e}"))?;
    let _ = socket.set_nodelay(true);
    socket
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .and_then(|()| socket.set_write_timeout(Some(HANDSHAKE_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    let flags = if mode.persistent { FLAG_PERSISTENT } else { 0 };
    let (init, m1) =
        pc::initiator_start(&psk, pc::PROLOGUE_LINK, flags, expect).map_err(|e| e.to_string())?;
    socket.write_all(MAGIC).map_err(|e| format!("send: {e}"))?;
    write_message(&mut socket, &m1).map_err(|e| format!("send: {e}"))?;
    let m2 = read_message(&mut socket).map_err(|e| {
        format!("no handshake answer ({e}): not a Susi channel port, or the peer does not hold this cluster.key")
    })?;
    let (m3, transport, peer) = init.finish(&local, &m2).map_err(|e| e.to_string())?;
    write_message(&mut socket, &m3).map_err(|e| format!("send: {e}"))?;
    classify(&peer)?;
    start_link(
        socket,
        target,
        peer,
        transport,
        LinkRole {
            dialed: true,
            relay_trusted: mode.hub,
            persistent: mode.persistent,
        },
    )
}

/// How a link came to be: who dialed, whether the far end relays, and
/// whether the dialer keeps it open.
#[derive(Clone, Copy)]
struct LinkRole {
    dialed: bool,
    relay_trusted: bool,
    persistent: bool,
}

fn start_link(
    socket: TcpStream,
    remote: SocketAddr,
    peer: PeerIdentity,
    transport: pc::Transport,
    role: LinkRole,
) -> Result<Arc<Link>, String> {
    let LinkRole {
        dialed,
        relay_trusted,
        persistent,
    } = role;
    let _ = socket.set_write_timeout(Some(WRITE_TIMEOUT));
    let reader = socket.try_clone().map_err(|e| e.to_string())?;
    let (send, recv) = transport.into_halves();
    let reg = registry();
    let link = Arc::new(Link {
        id: reg.next_id.fetch_add(1, Ordering::SeqCst),
        peer,
        remote,
        dialed,
        relay_trusted,
        persistent,
        writer: Mutex::new((socket, send)),
        streams: Mutex::new(HashMap::new()),
        next_sid: AtomicU32::new(1),
        last_rx: Mutex::new(Instant::now()),
        last_use: Mutex::new(Instant::now()),
        closed: AtomicBool::new(false),
    });
    reg.add(Arc::clone(&link));
    let runner = Arc::clone(&link);
    std::thread::Builder::new()
        .name("susi-channel-link".into())
        .spawn(move || runner.run_reader(reader, recv))
        .map_err(|e| {
            link.close("no reader thread");
            e.to_string()
        })?;
    Ok(link)
}

/// Whether `ip` is one of this machine's own addresses.
fn is_own_address(ip: std::net::IpAddr) -> bool {
    ip.to_canonical().is_loopback() || TcpListener::bind((ip, 0)).is_ok()
}

/// Run the responder handshake on an accepted connection and register the
/// link. `advertise` is this node's public address list, sent to the
/// dialer as a signed hint.
pub fn accept(mut socket: TcpStream, advertise: &[String]) -> Result<Arc<Link>, String> {
    let remote = socket.peer_addr().map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .and_then(|()| socket.set_write_timeout(Some(HANDSHAKE_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    let _ = socket.set_nodelay(true);
    let mut magic = [0u8; 8];
    socket
        .read_exact(&mut magic)
        .map_err(|e| format!("no channel preamble: {e}"))?;
    if &magic != MAGIC {
        return Err("not a Susi peer channel connection".to_string());
    }
    let local = local_identity()?;
    let psks = pc::cluster_psks();
    let m1 = read_message(&mut socket).map_err(|e| e.to_string())?;
    let (resp, m2, flags) = {
        let mut replay = registry()
            .link_replay
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        pc::responder_accept(&local, &psks, pc::PROLOGUE_LINK, &m1, &mut replay)
            .map_err(|e| e.to_string())?
    };
    write_message(&mut socket, &m2).map_err(|e| e.to_string())?;
    let m3 = read_message(&mut socket).map_err(|e| e.to_string())?;
    let (transport, peer) = resp.finish(&m3).map_err(|e| e.to_string())?;
    let class = classify(&peer)?;
    if class == PeerClass::SelfNode && !is_own_address(remote.ip()) {
        return Err(format!(
            "this node's own key presented from {remote}, which is not this machine"
        ));
    }
    let link = start_link(
        socket,
        remote,
        peer,
        transport,
        LinkRole {
            dialed: false,
            relay_trusted: false,
            persistent: flags & FLAG_PERSISTENT != 0,
        },
    )?;
    // A member that reconnects replaces its previous uplink here.
    if link.persistent {
        for old in registry().snapshot() {
            if old.id != link.id
                && old.persistent
                && !old.dialed
                && old.peer.pubkey.eq_ignore_ascii_case(&link.peer.pubkey)
            {
                old.close("replaced by a newer link from the same node");
            }
        }
    }
    if !advertise.is_empty() {
        if let Some(body) = signed_hint(advertise) {
            let _ = link.send(HINT, 0, &body);
        }
    }
    Ok(link)
}

/// Decides, per source address, whether a listener reads a connection.
pub type AcceptPolicy = Arc<dyn Fn(&SocketAddr) -> bool + Send + Sync>;

/// A [`DialPolicy`] a long-lived thread owns.
pub type SharedDialPolicy = Arc<dyn Fn(&SocketAddr) -> Result<(), String> + Send + Sync>;

/// Accept channel connections on `listener` until `stop` is set. `allow`
/// decides per source address before any byte is read (privacy posture);
/// `advertise` is passed to [`accept`].
pub fn serve_listener(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    allow: AcceptPolicy,
    advertise: Vec<String>,
) {
    let pending = Arc::new(AtomicU32::new(0));
    let advertise = Arc::new(advertise);
    let _ = listener.set_nonblocking(true);
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((socket, from)) => {
                if !allow(&from) {
                    drop(socket);
                    continue;
                }
                if pending.load(Ordering::SeqCst) as usize >= MAX_HANDSHAKES {
                    drop(socket);
                    continue;
                }
                let _ = socket.set_nonblocking(false);
                pending.fetch_add(1, Ordering::SeqCst);
                let in_flight = Arc::clone(&pending);
                let advertise = Arc::clone(&advertise);
                let spawned = std::thread::Builder::new()
                    .name("susi-channel-accept".into())
                    .spawn(move || {
                        if let Err(e) = accept(socket, &advertise) {
                            eprintln!("[CHANNEL] refused {from}: {e}");
                        }
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("[CHANNEL] accept failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

// ---- streams and end-to-end sessions ----

/// One end of a stream on a link: end-to-end messages in and out.
struct StreamEnd {
    link: Arc<Link>,
    sid: u32,
    rx: mpsc::Receiver<Event>,
    closed: Arc<AtomicBool>,
}

impl StreamEnd {
    fn new(link: Arc<Link>, sid: u32, rx: mpsc::Receiver<Event>) -> Self {
        Self {
            link,
            sid,
            rx,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn send(&self, msg: &[u8]) -> Result<(), String> {
        self.link.touch();
        self.link.send(DATA, self.sid, msg)
    }

    fn recv(&self, timeout: Duration) -> Result<Vec<u8>, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(Event::Data(d)) => Ok(d),
            Ok(Event::Closed(reason)) => Err(format!("stream closed: {reason}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("no answer within {}s", timeout.as_secs()))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("stream closed".to_string()),
        }
    }
}

/// Send CLOSE once and forget the stream.
fn close_stream(link: &Link, sid: u32, closed: &AtomicBool) {
    if closed.swap(true, Ordering::SeqCst) {
        return;
    }
    if link.take_slot(sid).is_some() {
        let _ = link.send(CLOSE, sid, b"");
    }
}

/// Terminate an incoming end-to-end session and hand it to the service.
fn serve_incoming(link: Arc<Link>, end: StreamEnd, src: String) {
    let sid = end.sid;
    let closed = Arc::clone(&end.closed);
    let remote = link.remote;
    let outcome = (move || -> Result<(ChannelStream, ChannelPeer), String> {
        let local = local_identity()?;
        let psks = pc::cluster_psks();
        let m1 = end.recv(HANDSHAKE_TIMEOUT)?;
        let (resp, m2, _) = {
            let mut replay = registry()
                .e2e_replay
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            pc::responder_accept(&local, &psks, pc::PROLOGUE_E2E, &m1, &mut replay)
                .map_err(|e| e.to_string())?
        };
        end.send(&m2)?;
        let m3 = end.recv(HANDSHAKE_TIMEOUT)?;
        let (transport, peer) = resp.finish(&m3).map_err(|e| e.to_string())?;
        if peer.node_id != src {
            return Err(format!(
                "stream labelled from {src} proved identity {}",
                peer.node_id
            ));
        }
        let class = classify(&peer)?;
        Ok((
            ChannelStream::new(end, transport, peer.clone()),
            ChannelPeer {
                identity: peer,
                class,
                link_source: remote,
            },
        ))
    })();
    match outcome {
        Ok((stream, peer)) => match registry().serve.get() {
            Some(hook) => hook(stream, peer),
            None => drop(stream),
        },
        Err(e) => {
            eprintln!("[CHANNEL] session refused: {e}");
            close_stream(&link, sid, &closed);
        }
    }
}

/// An established end-to-end session: a byte stream sealed between the two
/// end nodes. Dropping it closes the stream.
pub struct ChannelStream {
    reader: ChannelReader,
    writer: ChannelWriter,
}

/// The receiving half of a [`ChannelStream`].
pub struct ChannelReader {
    rx: mpsc::Receiver<Event>,
    cipher: CipherState,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    timeout: Option<Duration>,
    link: Arc<Link>,
    sid: u32,
    closed: Arc<AtomicBool>,
}

/// The sending half of a [`ChannelStream`].
pub struct ChannelWriter {
    cipher: CipherState,
    link: Arc<Link>,
    sid: u32,
    closed: Arc<AtomicBool>,
}

impl ChannelStream {
    fn new(end: StreamEnd, transport: pc::Transport, peer: PeerIdentity) -> Self {
        let _ = peer;
        let (send, recv) = transport.into_halves();
        let StreamEnd {
            link,
            sid,
            rx,
            closed,
        } = end;
        Self {
            reader: ChannelReader {
                rx,
                cipher: recv,
                buf: Vec::new(),
                pos: 0,
                eof: false,
                timeout: Some(Duration::from_secs(20)),
                link: Arc::clone(&link),
                sid,
                closed: Arc::clone(&closed),
            },
            writer: ChannelWriter {
                cipher: send,
                link,
                sid,
                closed,
            },
        }
    }

    /// Bound on each blocking read; `None` waits as long as the link lives.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.reader.timeout = timeout;
    }

    /// The two halves, for a reader thread and a writer thread.
    pub fn split(self) -> (ChannelReader, ChannelWriter) {
        (self.reader, self.writer)
    }
}

impl ChannelReader {
    /// Bound on each blocking read.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.eof || out.is_empty() {
                return Ok(0);
            }
            let event = match self.timeout {
                Some(t) => self.rx.recv_timeout(t).map_err(|e| match e {
                    mpsc::RecvTimeoutError::Timeout => std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "peer channel read timed out",
                    ),
                    mpsc::RecvTimeoutError::Disconnected => {
                        std::io::Error::from(std::io::ErrorKind::UnexpectedEof)
                    }
                }),
                None => self
                    .rx
                    .recv()
                    .map_err(|_| std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            };
            match event {
                Ok(Event::Data(sealed)) => {
                    let plain = self.cipher.decrypt(&[], &sealed).map_err(|e| {
                        close_stream(&self.link, self.sid, &self.closed);
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    })?;
                    self.buf = plain;
                    self.pos = 0;
                }
                Ok(Event::Closed(_)) => self.eof = true,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => self.eof = true,
                Err(e) => return Err(e),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        let n = data.len().min(APP_CHUNK);
        let sealed = self
            .cipher
            .encrypt(&[], &data[..n])
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.link.touch();
        self.link
            .send(DATA, self.sid, &sealed)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        close_stream(&self.link, self.sid, &self.closed);
    }
}

impl Drop for ChannelReader {
    fn drop(&mut self) {
        close_stream(&self.link, self.sid, &self.closed);
    }
}

impl Read for ChannelStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(out)
    }
}

impl Write for ChannelStream {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.writer.write(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// Pump a terminated session and a local socket into each other until
/// either side ends: the peer's bytes go to `socket` (whose write half is
/// shut when the peer closes), the socket's bytes go back to the peer
/// (whose stream is closed when the socket ends). `idle` bounds each wait
/// for the peer's next bytes. Blocks the calling thread; one helper thread
/// carries the other direction.
pub fn bridge(stream: ChannelStream, socket: TcpStream, idle: Duration) {
    let (mut from_peer, mut to_peer) = stream.split();
    from_peer.set_read_timeout(Some(idle));
    let Ok(mut to_socket) = socket.try_clone() else {
        return;
    };
    let mut from_socket = socket;
    let inbound = std::thread::Builder::new()
        .name("susi-channel-in".into())
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match from_peer.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if to_socket.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = to_socket.shutdown(std::net::Shutdown::Write);
        });
    if inbound.is_err() {
        return;
    }
    let mut buf = vec![0u8; APP_CHUNK];
    loop {
        match from_socket.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to_peer.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = from_socket.shutdown(std::net::Shutdown::Both);
    drop(to_peer);
}

/// Run the end-to-end initiator handshake on a fresh stream.
fn start_session(end: StreamEnd, want: &str, timeout: Duration) -> Result<ChannelStream, String> {
    let local = local_identity()?;
    let psk = first_psk()?;
    let fail = |end: &StreamEnd, e: String| {
        close_stream(&end.link, end.sid, &end.closed);
        e
    };
    let (init, m1) = pc::initiator_start(&psk, pc::PROLOGUE_E2E, 0, Expect::Key(want.to_string()))
        .map_err(|e| e.to_string())?;
    end.send(&m1).map_err(|e| fail(&end, e))?;
    let m2 = end.recv(timeout).map_err(|e| fail(&end, e))?;
    let (m3, transport, peer) = match init.finish(&local, &m2) {
        Ok(v) => v,
        Err(e) => return Err(fail(&end, e.to_string())),
    };
    end.send(&m3).map_err(|e| fail(&end, e))?;
    Ok(ChannelStream::new(end, transport, peer))
}

// ---- client API ----

/// Who a call is for: the node id and key it must prove, and addresses where
/// that node itself may listen.
#[derive(Debug, Clone)]
pub struct Target {
    pub node_id: String,
    /// Ed25519 public key (hex) the destination must prove.
    pub pubkey: String,
    /// `host:port` channel endpoints of the destination itself.
    pub direct: Vec<String>,
}

/// A hub row of [`HUBS_FILE`].
#[derive(Debug, Clone)]
pub struct Hub {
    pub node_id: String,
    pub pubkey: String,
    /// Addresses to dial, in order: the operator's address, then the hub's
    /// newest signed hints.
    pub addresses: Vec<String>,
}

/// The hubs this node keeps uplinks to.
pub fn hubs() -> Vec<Hub> {
    crate::cluster_key::config_json_rows(HUBS_FILE)
        .iter()
        .filter_map(|row| {
            let node_id = row.get("node_id")?.as_str()?.to_string();
            let pubkey = row.get("pubkey")?.as_str()?.to_string();
            let mut addresses: Vec<String> = row
                .get("address")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .into_iter()
                .collect();
            if let Some(hints) = row.get("hints").and_then(|v| v.as_array()) {
                for h in hints.iter().filter_map(|v| v.as_str()) {
                    if !addresses.iter().any(|a| a == h) {
                        addresses.push(h.to_string());
                    }
                }
            }
            (!pubkey.is_empty()).then_some(Hub {
                node_id,
                pubkey,
                addresses,
            })
        })
        .collect()
}

/// Record `hub` (identity first, address as a hint) in [`HUBS_FILE`],
/// replacing a row with the same key or node id.
pub fn remember_hub(node_id: &str, pubkey: &str, address: &str) -> Result<(), String> {
    let dir = susi_paths::SusiDirs::config_dir();
    let Some(_lock) = crate::file_lock::FileLock::acquire(&dir, "peer_channel_hubs") else {
        return Err("peer_channel_hubs.json is locked by another writer".to_string());
    };
    let mut rows = crate::cluster_key::config_json_rows(HUBS_FILE);
    rows.retain(|r| {
        r.get("node_id").and_then(|v| v.as_str()) != Some(node_id)
            && !r
                .get("pubkey")
                .and_then(|v| v.as_str())
                .is_some_and(|k| k.eq_ignore_ascii_case(pubkey))
    });
    rows.push(serde_json::json!({"node_id": node_id, "pubkey": pubkey, "address": address}));
    write_rows(&dir.join(HUBS_FILE), &rows)
}

fn update_hub_hints(
    hub: &PeerIdentity,
    ts: u64,
    addrs: &[String],
    sig: &str,
) -> Result<(), String> {
    let dir = susi_paths::SusiDirs::config_dir();
    let Some(_lock) = crate::file_lock::FileLock::acquire(&dir, "peer_channel_hubs") else {
        return Err("peer_channel_hubs.json is locked by another writer".to_string());
    };
    let mut rows = crate::cluster_key::config_json_rows(HUBS_FILE);
    let mut changed = false;
    for row in &mut rows {
        let same = row
            .get("pubkey")
            .and_then(|v| v.as_str())
            .is_some_and(|k| k.eq_ignore_ascii_case(&hub.pubkey));
        let newer = row.get("hint_ts").and_then(|v| v.as_u64()).unwrap_or(0) < ts;
        if same && newer {
            row["hints"] = serde_json::json!(addrs);
            row["hint_ts"] = serde_json::json!(ts);
            row["hint_sig"] = serde_json::json!(sig);
            changed = true;
        }
    }
    if changed {
        write_rows(&dir.join(HUBS_FILE), &rows)?;
    }
    Ok(())
}

fn write_rows(path: &std::path::Path, rows: &[serde_json::Value]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(rows).map_err(|e| e.to_string())?;
    crate::atomic_write_bytes(path, text.as_bytes()).map_err(|e| e.to_string())
}

/// The address this node's own daemon accepts channel connections on, for a
/// local client to dial (an unspecified bind becomes loopback).
pub fn local_listen_addr() -> Option<String> {
    let cfg = crate::SusiConfig::load_global().ok()?;
    let raw = cfg.get::<String>("peer_channel_listen")?;
    let addr = parse_listen(&raw)?;
    let ip = if addr.ip().is_unspecified() {
        if addr.is_ipv4() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
    } else {
        addr.ip()
    };
    Some(SocketAddr::new(ip, addr.port()).to_string())
}

/// Parse `peer_channel_listen`: `ip:port`, or a bare IP on [`DEFAULT_PORT`].
/// Empty means no listener.
pub fn parse_listen(raw: &str) -> Option<SocketAddr> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<SocketAddr>().ok().or_else(|| {
        raw.parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
    })
}

fn ephemeral(hub: bool) -> DialMode {
    DialMode {
        persistent: false,
        hub,
    }
}

/// Open an end-to-end session to `target`, by the first route that works:
/// a link to it this process already holds, its own endpoints, this
/// process's uplinks, the hubs in [`HUBS_FILE`], then this node's own
/// daemon. Every route ends in the same end-to-end handshake, which only
/// `target.pubkey` can complete.
pub fn open(
    target: &Target,
    timeout: Duration,
    policy: DialPolicy<'_>,
) -> Result<ChannelStream, String> {
    let mut errors: Vec<String> = Vec::new();
    let attempt = |link: &Arc<Link>, how: &str, errors: &mut Vec<String>| match link
        .open_stream(&target.node_id)
        .and_then(|end| start_session(end, &target.pubkey, timeout))
    {
        Ok(s) => Some(s),
        Err(e) => {
            errors.push(format!("{how}: {e}"));
            None
        }
    };
    let held = registry().snapshot();
    if let Some(link) = held
        .iter()
        .rev()
        .find(|l| l.peer.pubkey.eq_ignore_ascii_case(&target.pubkey))
    {
        if let Some(s) = attempt(link, "held link", &mut errors) {
            return Ok(s);
        }
    }
    for addr in &target.direct {
        match dial(
            addr,
            Expect::Key(target.pubkey.clone()),
            ephemeral(false),
            policy,
        ) {
            Ok(link) => {
                if let Some(s) = attempt(&link, addr, &mut errors) {
                    return Ok(s);
                }
            }
            Err(e) => errors.push(e),
        }
    }
    for link in held.iter().filter(|l| l.is_uplink()) {
        if let Some(s) = attempt(link, &format!("via hub {}", link.peer.node_id), &mut errors) {
            return Ok(s);
        }
    }
    for hub in hubs() {
        // A short-lived link to this hub that an earlier call of this
        // process opened is reused before a new one is dialed.
        if let Some(held_hub) = registry().snapshot().into_iter().find(|l| {
            l.dialed && l.relay_trusted && l.peer.pubkey.eq_ignore_ascii_case(&hub.pubkey)
        }) {
            if let Some(s) = attempt(&held_hub, &format!("via hub {}", hub.node_id), &mut errors) {
                return Ok(s);
            }
        }
        for addr in &hub.addresses {
            match dial(
                addr,
                Expect::Key(hub.pubkey.clone()),
                ephemeral(true),
                policy,
            ) {
                Ok(link) => {
                    if let Some(s) =
                        attempt(&link, &format!("via hub {}", hub.node_id), &mut errors)
                    {
                        return Ok(s);
                    }
                    break;
                }
                Err(e) => errors.push(e),
            }
        }
    }
    if let (Some(addr), Some(own)) = (local_listen_addr(), crate::cluster_key::node_pubkey_hex()) {
        match dial(&addr, Expect::Key(own), ephemeral(true), policy) {
            Ok(link) => {
                if let Some(s) = attempt(&link, "via this node's daemon", &mut errors) {
                    return Ok(s);
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        errors.push(
            "no route: no link, endpoint, hub (peer_channel_hubs.json) or local channel listener"
                .to_string(),
        );
    }
    Err(format!(
        "peer channel to {} failed: {}",
        target.node_id,
        errors.join("; ")
    ))
}

/// Keep a persistent uplink to every hub in [`HUBS_FILE`] until `stop` is
/// set: reconnect with exponential backoff (1 s up to 60 s), re-resolving
/// names on every attempt.
pub fn keep_uplinks(stop: Arc<AtomicBool>, policy: SharedDialPolicy) {
    let mut backoff: HashMap<String, (Instant, Duration)> = HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        let held = registry().snapshot();
        for hub in hubs() {
            let up = held
                .iter()
                .any(|l| l.is_uplink() && l.peer.pubkey.eq_ignore_ascii_case(&hub.pubkey));
            if up {
                backoff.remove(&hub.pubkey);
                continue;
            }
            let (due, wait) = backoff
                .get(&hub.pubkey)
                .copied()
                .unwrap_or((Instant::now(), Duration::from_secs(1)));
            if Instant::now() < due {
                continue;
            }
            let mut connected = false;
            for addr in &hub.addresses {
                let mode = DialMode {
                    persistent: true,
                    hub: true,
                };
                match dial(addr, Expect::Key(hub.pubkey.clone()), mode, &*policy) {
                    Ok(_) => {
                        eprintln!("[CHANNEL] uplink to hub {} via {addr}", hub.node_id);
                        connected = true;
                        break;
                    }
                    Err(e) => eprintln!("[CHANNEL] uplink to hub {}: {e}", hub.node_id),
                }
            }
            if connected {
                backoff.remove(&hub.pubkey);
            } else {
                let next = (wait * 2).min(Duration::from_secs(60));
                backoff.insert(hub.pubkey.clone(), (Instant::now() + wait, next));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_bodies_roundtrip_and_reject_truncation() {
        let body = open_body(4, "susi-node-b", "susi-node-a");
        assert_eq!(
            parse_open(&body),
            Some((4, "susi-node-b".to_string(), "susi-node-a".to_string()))
        );
        assert_eq!(parse_open(&body[..body.len() - 1]), None);
        assert_eq!(parse_open(&[]), None);
    }

    #[test]
    fn listen_addresses_default_the_channel_port() {
        assert_eq!(
            parse_listen("0.0.0.0"),
            Some(SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)))
        );
        assert_eq!(
            parse_listen("192.168.0.140:29100"),
            Some(SocketAddr::from(([192, 168, 0, 140], 29_100)))
        );
        assert_eq!(parse_listen("  "), None);
        assert_eq!(parse_listen("not an address"), None);
    }

    #[test]
    fn the_default_channel_port_is_outside_every_contract_block() {
        // 9090-9095 shifted by any offset up to 2000, and the leaf ports.
        assert!(!(9_090..=11_095).contains(&DEFAULT_PORT));
        assert!(!(18_080..=18_084).contains(&DEFAULT_PORT));
    }

    #[test]
    fn framing_roundtrips_over_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_message(&mut client, b"sealed bytes").unwrap();
        assert_eq!(read_message(&mut server).unwrap(), b"sealed bytes");
        assert!(write_message(&mut client, &vec![0u8; 70_000]).is_err());
    }

    #[test]
    fn hint_payloads_bind_node_time_and_addresses() {
        let a = hint_payload("susi-node-h", 10, &["h.example:29090".to_string()]);
        let b = hint_payload("susi-node-h", 11, &["h.example:29090".to_string()]);
        assert_ne!(a, b);
        assert!(a.starts_with("susi-peer-channel-v1/hint:susi-node-h:10:"));
    }
}
