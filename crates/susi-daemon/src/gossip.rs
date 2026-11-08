//! Gossip Protocol for Peer-to-Peer Capability Propagation
//!
//! Epidemic capability routing over UDP. Datagrams are HMAC-SHA256 sealed
//! with a *per-peer* key derived from the cluster-key `susi-gossip-v1` MAC
//! (`susi-gossip-peer-v1:{node_id}`) so a packet destined for A cannot be
//! replayed onto B. Holders of `cluster.key` can still mint a valid MAC for
//! any id — this is destination-binding, not a per-peer secret. The capability
//! store `gossip_caps.json` is HMAC-sealed (`susi-gossip-store-v1`); unsigned
//! files are ignored.
//!
//! Under the `host_only` privacy posture gossip serves this machine alone:
//! the socket is bound to loopback (re-scoped by the recv loop within
//! [`SCOPE_RECHECK`] of a mode switch, no restart), a datagram from another
//! address is dropped unread even inside that window, and nothing is sent
//! to another host.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

const GOSSIP_LABEL: &[u8] = b"susi-gossip-v1";
const PEER_LABEL: &[u8] = b"susi-gossip-peer-v1:";
const STORE_LABEL: &[u8] = b"susi-gossip-store-v1";
const MAC_LEN: usize = 32;
/// How often the recv loop re-reads the privacy posture to re-scope the
/// socket (loopback under `host_only`, the requested address otherwise).
pub const SCOPE_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);

/// A gossip message exchanged between Swarm OS nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    /// Advertises capabilities available at a specific cell.
    AdvertiseCapabilities {
        cell_id: String,
        capabilities: Vec<String>,
        timestamp: u64,
    },
    /// Requests peer routing table.
    PeerDiscovery { from_ip: String, from_id: String },
}

/// Manages epidemic gossip state and UDP binding.
#[derive(Clone)]
pub struct GossipManager {
    /// Maps cell_id to a list of known capabilities.
    pub peer_capabilities: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// UDP socket for broadcasting / receiving. Behind a lock so the recv
    /// loop can re-scope it when the privacy posture changes; senders hold
    /// the read guard for one send, so a dropped socket closes at once.
    socket: Arc<RwLock<Option<UdpSocket>>>,
    /// The address `bind` asked for, its port pinned to the one first
    /// bound: the scope outside `host_only`.
    requested: Arc<RwLock<Option<SocketAddr>>>,
    /// `None` when neither a cluster key nor OS entropy exists — sealing and
    /// ingress fail closed rather than falling back to a known zero key.
    mac_key: Option<Arc<[u8; 32]>>,
    store: Option<PathBuf>,
    rejected: Arc<AtomicU64>,
}

impl Default for GossipManager {
    fn default() -> Self {
        Self::new()
    }
}

fn local_node_id() -> String {
    crate::susi_config::cluster_key::wire_node_id()
}

fn mac_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn hmac(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    crate::susi_config::cluster_key::hmac_sha256(key, msg)
}

fn process_mac_key() -> Option<[u8; 32]> {
    static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    *KEY.get_or_init(|| {
        if let Some(cluster) = crate::susi_config::cluster_key::cluster_key() {
            return Some(crate::susi_config::cluster_key::hmac_sha256(
                &cluster,
                GOSSIP_LABEL,
            ));
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).ok().map(|_| key)
    })
}

impl GossipManager {
    pub fn new() -> Self {
        Self {
            peer_capabilities: Arc::new(RwLock::new(HashMap::new())),
            socket: Arc::new(RwLock::new(None)),
            requested: Arc::new(RwLock::new(None)),
            mac_key: process_mac_key().map(Arc::new),
            store: None,
            rejected: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Load persisted capabilities from `path` (created on first persist).
    pub fn with_store(path: PathBuf) -> Self {
        let mut manager = Self::new();
        manager.store = Some(path.clone());
        if let Ok(bytes) = std::fs::read(&path)
            && let Some(map) = manager.open_store(&bytes)
        {
            let mut caps = manager
                .peer_capabilities
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for (cell, list) in map {
                caps.insert(cell, list.into_iter().collect());
            }
        }
        manager
    }

    /// `cluster` when `cluster.key` sealed the MAC; `local` for the process
    /// fallback; `none` when no key could be obtained (gossip inert).
    #[must_use]
    pub fn auth_mode() -> &'static str {
        if crate::susi_config::cluster_key::cluster_key().is_some() {
            "cluster"
        } else if process_mac_key().is_some() {
            "local"
        } else {
            "none"
        }
    }

    /// Unsigned or MAC-mismatched datagrams rejected since bind.
    #[must_use]
    pub fn rejected_count(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// Known cell count in the in-memory (and persisted) table.
    #[must_use]
    pub fn peer_count(&self) -> usize {
        self.peer_capabilities
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// HMAC-SHA256 seal destined to `peer_id` (per-peer MAC). `None` when no
    /// MAC key exists — callers skip the send rather than emit a forgeable
    /// datagram.
    #[must_use]
    pub fn seal_for(&self, peer_id: &str, body: &[u8]) -> Option<Vec<u8>> {
        let mac = hmac(&self.key_for(peer_id)?, body);
        let mut out = Vec::with_capacity(MAC_LEN + body.len());
        out.extend_from_slice(&mac);
        out.extend_from_slice(body);
        Some(out)
    }

    /// Seal for this host (tests and loopback ingest).
    #[must_use]
    pub fn seal(&self, body: &[u8]) -> Option<Vec<u8>> {
        self.seal_for(&local_node_id(), body)
    }

    fn key_for(&self, peer_id: &str) -> Option<[u8; 32]> {
        let key = self.mac_key.as_deref()?;
        let mut msg = Vec::with_capacity(PEER_LABEL.len() + peer_id.len());
        msg.extend_from_slice(PEER_LABEL);
        msg.extend_from_slice(peer_id.as_bytes());
        Some(hmac(key, &msg))
    }

    fn store_key(&self) -> Option<[u8; 32]> {
        self.mac_key.as_deref().map(|key| hmac(key, STORE_LABEL))
    }

    fn open<'a>(&self, payload: &'a [u8]) -> Result<&'a [u8], String> {
        let mac = payload.get(..MAC_LEN).ok_or("unsigned gossip datagram")?;
        let body = payload.get(MAC_LEN..).ok_or("truncated gossip datagram")?;
        let expected = hmac(
            &self
                .key_for(&local_node_id())
                .ok_or("no gossip MAC key (no cluster key, no OS entropy)")?,
            body,
        );
        if !mac_eq(mac, &expected) {
            return Err("gossip HMAC mismatch".to_string());
        }
        Ok(body)
    }

    fn open_store(&self, bytes: &[u8]) -> Option<HashMap<String, Vec<String>>> {
        let text = std::str::from_utf8(bytes).ok()?;
        let wrap: serde_json::Value = serde_json::from_str(text).ok()?;
        let v = wrap.get("v")?.as_u64()?;
        if v != 1 {
            return None;
        }
        let mac_hex = wrap.get("mac")?.as_str()?;
        let caps = wrap.get("caps")?;
        let caps_bytes = serde_json::to_vec(caps).ok()?;
        let expected = hmac(&self.store_key()?, &caps_bytes);
        let got = hex::decode(mac_hex).ok()?;
        if !mac_eq(&got, &expected) {
            return None;
        }
        serde_json::from_value(caps.clone()).ok()
    }

    fn persist(&self) {
        let Some(path) = &self.store else {
            return;
        };
        let map = self
            .peer_capabilities
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let json: BTreeMap<String, Vec<String>> = map
            .iter()
            .map(|(k, v)| {
                let mut caps: Vec<String> = v.iter().cloned().collect();
                caps.sort();
                (k.clone(), caps)
            })
            .collect();
        if let Ok(caps_value) = serde_json::to_value(&json)
            && let Ok(caps_bytes) = serde_json::to_vec(&caps_value)
            && let Some(store_key) = self.store_key()
        {
            let mac = hmac(&store_key, &caps_bytes);
            let wrap = serde_json::json!({
                "v": 1,
                "mac": hex::encode(mac),
                "caps": caps_value,
            });
            if let Ok(text) = serde_json::to_string_pretty(&wrap) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
    }

    /// Binds the gossip manager to a local UDP port: `addr`, or its
    /// family's loopback address while `host_only` is in force.
    pub fn bind(&mut self, addr: &str) -> std::io::Result<()> {
        self.bind_scoped(addr, susi_core::mac_policy::host_only_in_force())
    }

    /// [`bind`](Self::bind) with the posture given: loopback when
    /// `host_only`, `addr` otherwise.
    pub fn bind_scoped(&mut self, addr: &str, host_only: bool) -> std::io::Result<()> {
        let requested = addr.to_socket_addrs()?.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{addr} names no socket address"),
            )
        })?;
        let socket = open_udp(scoped(requested, host_only))?;
        let requested = SocketAddr::new(requested.ip(), socket.local_addr()?.port());
        *self.requested.write().unwrap_or_else(|e| e.into_inner()) = Some(requested);
        *self.socket.write().unwrap_or_else(|e| e.into_inner()) = Some(socket);
        Ok(())
    }

    /// Re-scope the bound socket to the posture: loopback while
    /// `host_only`, the requested address otherwise. `Ok(true)` when it
    /// re-bound. The old socket closes before the new one binds (a wildcard
    /// and a specific bind of one port conflict); a failed bind leaves
    /// gossip unbound, never on the wider scope, and the next call retries.
    pub fn align_scope(&self, host_only: bool) -> std::io::Result<bool> {
        let Some(requested) = *self.requested.read().unwrap_or_else(|e| e.into_inner()) else {
            return Ok(false);
        };
        let want = scoped(requested, host_only);
        let mut slot = self.socket.write().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().and_then(|s| s.local_addr().ok()) == Some(want) {
            return Ok(false);
        }
        *slot = None;
        *slot = Some(open_udp(want)?);
        Ok(true)
    }

    /// Bound socket address, if `bind` succeeded.
    pub fn local_addr(&self) -> Option<String> {
        self.socket
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.to_string())
    }

    /// Handles an incoming sealed gossip payload.
    pub fn handle_gossip(&self, payload: &[u8]) -> Result<(), String> {
        self.ingest(payload, None)
    }

    /// Ingest a sealed payload, optionally advertising back to `from` on discovery.
    pub fn ingest(&self, payload: &[u8], from: Option<&str>) -> Result<(), String> {
        let body = match self.open(payload) {
            Ok(b) => b,
            Err(e) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                return Err(e);
            }
        };
        let msg: GossipMessage = serde_json::from_slice(body).map_err(|e| e.to_string())?;

        match msg {
            GossipMessage::AdvertiseCapabilities {
                cell_id,
                capabilities,
                ..
            } => {
                let mut map = self
                    .peer_capabilities
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let entry = map.entry(cell_id).or_default();
                for cap in capabilities {
                    entry.insert(cap);
                }
                drop(map);
                self.persist();
            }
            GossipMessage::PeerDiscovery { from_id, .. } => {
                if let Some(addr) = from
                    && !from_id.is_empty()
                {
                    let _ = self.advertise(
                        addr,
                        &from_id,
                        &local_node_id(),
                        vec!["host".to_string(), "infer".to_string()],
                    );
                }
            }
        }

        Ok(())
    }

    /// One non-blocking recv. `Ok(true)` means a datagram was consumed:
    /// ingested, or dropped unread because it came from another host while
    /// `host_only` is in force.
    pub fn poll_recv(&self) -> std::io::Result<bool> {
        let mut buf = [0u8; 2048];
        let received = {
            let slot = self.socket.read().unwrap_or_else(|e| e.into_inner());
            let Some(socket) = slot.as_ref() else {
                return Ok(false);
            };
            socket.recv_from(&mut buf)
        };
        match received {
            Ok((n, src)) => {
                if !src.ip().to_canonical().is_loopback()
                    && susi_core::mac_policy::host_only_in_force()
                {
                    return Ok(true);
                }
                let _ = self.ingest(&buf[..n], Some(&src.to_string()));
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Recv loop for the daemon process lifetime — supervised like every
    /// other long-lived worker: heartbeated, bounded-restart, reported.
    pub fn spawn_recv_loop(&self) {
        let this = self.clone();
        crate::service_supervision::Supervisor::global().spawn(
            "gossip-recv",
            3,
            Some(std::time::Duration::from_secs(120)),
            move || {
                let this = this.clone();
                Some(move || {
                    let mut scoped_at: Option<std::time::Instant> = None;
                    let mut last_scope_error: Option<String> = None;
                    loop {
                        crate::service_supervision::heartbeat("gossip-recv");
                        if scoped_at.is_none_or(|at| at.elapsed() >= SCOPE_RECHECK) {
                            let host_only = susi_core::mac_policy::host_only_in_force();
                            match this.align_scope(host_only) {
                                Ok(true) => {
                                    last_scope_error = None;
                                    eprintln!(
                                        "[gossip] privacy posture {}: listening on {}",
                                        if host_only {
                                            "host_only"
                                        } else {
                                            "not host_only"
                                        },
                                        this.local_addr().unwrap_or_default()
                                    );
                                }
                                Ok(false) => last_scope_error = None,
                                Err(error) => {
                                    let error = error.to_string();
                                    if last_scope_error.as_deref() != Some(error.as_str()) {
                                        eprintln!(
                                            "[gossip] could not re-scope the socket: {error}"
                                        );
                                        last_scope_error = Some(error);
                                    }
                                }
                            }
                            scoped_at = Some(std::time::Instant::now());
                        }
                        match this.poll_recv() {
                            Ok(true) => {}
                            Ok(false) => std::thread::sleep(std::time::Duration::from_millis(50)),
                            Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
                        }
                    }
                })
            },
        );
    }

    /// Rewrite a cluster peer host:port onto the gossip UDP port.
    #[must_use]
    pub fn gossip_target(peer_addr: &str, gossip_port: u16) -> String {
        if let Ok(mut sa) = peer_addr.parse::<std::net::SocketAddr>() {
            sa.set_port(gossip_port);
            return sa.to_string();
        }
        match peer_addr.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                format!("{host}:{gossip_port}")
            }
            _ => format!("{peer_addr}:{gossip_port}"),
        }
    }

    /// Fan out host capabilities (and a discovery ping) to verified cluster peers.
    /// Nothing goes out while `host_only` is in force: every member is
    /// another host.
    pub fn fanout_to_cluster(&self, gossip_port: u16) {
        if susi_core::mac_policy::host_only_in_force() {
            return;
        }
        let Ok(peers) = susi_core::plane_bus::gawd::cluster_peers() else {
            return;
        };
        let from_ip = self.local_addr().unwrap_or_default();
        let us = local_node_id();
        for (id, addr) in peers {
            let target = Self::gossip_target(&addr, gossip_port);
            let _ = self.advertise(
                &target,
                &id,
                &us,
                vec!["host".to_string(), "infer".to_string()],
            );
            if let Some(socket) = self
                .socket
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                let msg = GossipMessage::PeerDiscovery {
                    from_ip: from_ip.clone(),
                    from_id: us.clone(),
                };
                if let Ok(body) = serde_json::to_vec(&msg)
                    && let Some(payload) = self.seal_for(&id, &body)
                {
                    let _ = socket.send_to(&payload, &target);
                }
            }
        }
        self.persist();
    }

    /// Advertises this node's capabilities to a known peer. Refused for a
    /// target on another host while `host_only` is in force.
    pub fn advertise(
        &self,
        target_addr: &str,
        peer_id: &str,
        cell_id: &str,
        capabilities: Vec<String>,
    ) -> std::io::Result<()> {
        if !target_on_this_machine(target_addr) && susi_core::mac_policy::host_only_in_force() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("host-only privacy mode: no gossip to {target_addr} (another host)"),
            ));
        }
        if let Some(socket) = self
            .socket
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let msg = GossipMessage::AdvertiseCapabilities {
                cell_id: cell_id.to_string(),
                capabilities,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            let body = serde_json::to_vec(&msg)?;
            let payload = self
                .seal_for(peer_id, &body)
                .ok_or_else(|| std::io::Error::other("no gossip MAC key"))?;
            socket.send_to(&payload, target_addr)?;
        }
        Ok(())
    }

    /// Returns whether a specific peer has a given capability.
    pub fn peer_has_capability(&self, cell_id: &str, capability: &str) -> bool {
        let map = self
            .peer_capabilities
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(caps) = map.get(cell_id) {
            caps.contains(capability)
        } else {
            false
        }
    }
}

/// `requested`, or its family's loopback address (same port) under
/// `host_only`.
fn scoped(requested: SocketAddr, host_only: bool) -> SocketAddr {
    if !host_only || requested.ip().is_loopback() {
        return requested;
    }
    let loopback = match requested {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    SocketAddr::new(loopback, requested.port())
}

fn open_udp(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind(addr)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// Whether a `host:port` gossip target is this machine (loopback or
/// `localhost`).
fn target_on_this_machine(target: &str) -> bool {
    match target.parse::<SocketAddr>() {
        Ok(addr) => addr.ip().to_canonical().is_loopback(),
        Err(_) => target
            .rsplit_once(':')
            .is_some_and(|(host, _)| susi_core::mac_policy::host_is_this_machine(host)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This host's routed non-loopback IPv4 address, if it has one.
    fn routed_ipv4() -> Option<IpAddr> {
        let probe = UdpSocket::bind("0.0.0.0:0").ok()?;
        probe.connect("192.0.2.1:9").ok()?;
        let ip = probe.local_addr().ok()?.ip();
        (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
    }

    #[test]
    fn host_only_scopes_the_socket_to_loopback_and_back() {
        let mut manager = GossipManager::new();
        manager.bind_scoped("0.0.0.0:0", false).unwrap();
        let wide: SocketAddr = manager.local_addr().unwrap().parse().unwrap();
        assert!(wide.ip().is_unspecified(), "{wide}");

        assert!(manager.align_scope(true).unwrap());
        let narrow: SocketAddr = manager.local_addr().unwrap().parse().unwrap();
        assert_eq!(narrow, SocketAddr::from(([127, 0, 0, 1], wide.port())));
        assert!(!manager.align_scope(true).unwrap(), "already scoped");
        // A loopback sender still reaches the narrowed socket.
        let local = UdpSocket::bind("127.0.0.1:0").unwrap();
        local.send_to(b"local", narrow).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !manager.poll_recv().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "loopback datagram never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Another host cannot reach it at all: nothing is listening there.
        match routed_ipv4() {
            Some(ip) => {
                let sender = UdpSocket::bind((ip, 0)).unwrap();
                let _ = sender.send_to(b"from-the-lan", (ip, wide.port()));
                std::thread::sleep(std::time::Duration::from_millis(300));
                assert!(
                    !manager.poll_recv().unwrap(),
                    "an off-host datagram arrived"
                );
            }
            None => eprintln!("no routed non-loopback IPv4 on this host: off-host leg skipped"),
        }

        assert!(manager.align_scope(false).unwrap());
        assert_eq!(manager.local_addr().unwrap(), wide.to_string());
        // Booting under host_only binds loopback from the start.
        let mut boot = GossipManager::new();
        boot.bind_scoped("0.0.0.0:0", true).unwrap();
        let addr: SocketAddr = boot.local_addr().unwrap().parse().unwrap();
        assert!(addr.ip().is_loopback(), "{addr}");
    }

    #[test]
    fn gossip_targets_on_this_machine_are_loopback_or_localhost() {
        assert!(target_on_this_machine("127.0.0.1:9095"));
        assert!(target_on_this_machine("[::1]:9095"));
        assert!(target_on_this_machine("localhost:9095"));
        assert!(!target_on_this_machine("192.168.1.4:9095"));
        assert!(!target_on_this_machine("[fe80::1]:9095"));
        assert!(!target_on_this_machine("node.local:9095"));
        assert!(!target_on_this_machine("node-alpha.internal:9095"));
    }

    #[test]
    fn test_gossip_capability_propagation() {
        let manager = GossipManager::new();

        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "remote-cell-1".to_string(),
            capabilities: vec!["infer".to_string(), "tool:git".to_string()],
            timestamp: 1600000000,
        };
        let body = serde_json::to_vec(&msg).unwrap();
        let payload = manager.seal(&body).unwrap();

        manager.handle_gossip(&payload).unwrap();

        assert!(manager.peer_has_capability("remote-cell-1", "infer"));
        assert!(manager.peer_has_capability("remote-cell-1", "tool:git"));
        assert!(!manager.peer_has_capability("remote-cell-1", "audit:seal"));
    }

    #[test]
    fn unsigned_datagrams_are_rejected() {
        let manager = GossipManager::new();
        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "spoof".to_string(),
            capabilities: vec!["infer".to_string()],
            timestamp: 1,
        };
        let body = serde_json::to_vec(&msg).unwrap();
        assert!(manager.handle_gossip(&body).is_err());
        assert!(!manager.peer_has_capability("spoof", "infer"));
        assert_eq!(manager.rejected_count(), 1);
    }

    #[test]
    fn gossip_target_rewrites_socket_and_hostname_ports() {
        assert_eq!(
            GossipManager::gossip_target("127.0.0.1:9090", 9099),
            "127.0.0.1:9099"
        );
        assert_eq!(
            GossipManager::gossip_target("node-alpha.internal:8080", 9099),
            "node-alpha.internal:9099"
        );
        assert_eq!(
            GossipManager::gossip_target("node-beta.internal", 9099),
            "node-beta.internal:9099"
        );
    }

    #[test]
    fn tampered_sealed_payload_is_rejected_and_increments_counter() {
        let manager = GossipManager::new();
        assert!(!manager.peer_has_capability("unknown-cell", "infer"));
        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "cell-tampered".to_string(),
            capabilities: vec!["infer".to_string()],
            timestamp: 42,
        };
        let body = serde_json::to_vec(&msg).unwrap();
        let mut sealed = manager.seal(&body).unwrap();
        if let Some(last) = sealed.last_mut() {
            *last ^= 0xFF;
        }
        assert!(manager.handle_gossip(&sealed).is_err());
        assert!(!manager.peer_has_capability("cell-tampered", "infer"));
        assert_eq!(manager.rejected_count(), 1);
    }

    #[test]
    fn with_store_persists_and_reloads_hmac_sealed_capabilities() {
        let path =
            std::env::temp_dir().join(format!("susi-gossip-store-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let manager = GossipManager::with_store(path.clone());
        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "cell-persisted".to_string(),
            capabilities: vec!["embed".to_string(), "rerank".to_string()],
            timestamp: 99,
        };
        let body = serde_json::to_vec(&msg).unwrap();
        let sealed = manager.seal(&body).unwrap();
        manager.handle_gossip(&sealed).unwrap();
        assert_eq!(manager.peer_count(), 1);

        let reloaded = GossipManager::with_store(path.clone());
        assert!(reloaded.peer_has_capability("cell-persisted", "embed"));
        assert!(reloaded.peer_has_capability("cell-persisted", "rerank"));
        let _ = std::fs::remove_file(&path);
    }
}
