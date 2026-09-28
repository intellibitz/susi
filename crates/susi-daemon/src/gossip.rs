//! Gossip Protocol for Peer-to-Peer Capability Propagation
//!
//! Epidemic capability routing over UDP. Datagrams are HMAC-SHA256 sealed
//! with a *per-peer* key derived from the cluster-key `susi-gossip-v1` MAC
//! (`susi-gossip-peer-v1:{node_id}`) so a packet destined for A cannot be
//! replayed onto B. Holders of `cluster.key` can still mint a valid MAC for
//! any id — this is destination-binding, not a per-peer secret. The capability
//! store `gossip_caps.json` is HMAC-sealed (`susi-gossip-store-v1`); unsigned
//! files are ignored.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

const GOSSIP_LABEL: &[u8] = b"susi-gossip-v1";
const PEER_LABEL: &[u8] = b"susi-gossip-peer-v1:";
const STORE_LABEL: &[u8] = b"susi-gossip-store-v1";
const MAC_LEN: usize = 32;

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
    /// UDP socket for broadcasting / receiving.
    socket: Option<Arc<UdpSocket>>,
    mac_key: Arc<[u8; 32]>,
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

fn process_mac_key() -> [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    *KEY.get_or_init(|| {
        if let Some(cluster) = crate::susi_config::cluster_key::cluster_key() {
            crate::susi_config::cluster_key::hmac_sha256(&cluster, GOSSIP_LABEL)
        } else {
            let mut key = [0u8; 32];
            let _ = getrandom::fill(&mut key);
            key
        }
    })
}

impl GossipManager {
    pub fn new() -> Self {
        Self {
            peer_capabilities: Arc::new(RwLock::new(HashMap::new())),
            socket: None,
            mac_key: Arc::new(process_mac_key()),
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

    /// `cluster` when `cluster.key` sealed the MAC; `local` for the process fallback.
    #[must_use]
    pub fn auth_mode() -> &'static str {
        if crate::susi_config::cluster_key::cluster_key().is_some() {
            "cluster"
        } else {
            "local"
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

    /// HMAC-SHA256 seal destined to `peer_id` (per-peer MAC).
    #[must_use]
    pub fn seal_for(&self, peer_id: &str, body: &[u8]) -> Vec<u8> {
        let mac = hmac(&self.key_for(peer_id), body);
        let mut out = Vec::with_capacity(MAC_LEN + body.len());
        out.extend_from_slice(&mac);
        out.extend_from_slice(body);
        out
    }

    /// Seal for this host (tests and loopback ingest).
    #[must_use]
    pub fn seal(&self, body: &[u8]) -> Vec<u8> {
        self.seal_for(&local_node_id(), body)
    }

    fn key_for(&self, peer_id: &str) -> [u8; 32] {
        let mut msg = Vec::with_capacity(PEER_LABEL.len() + peer_id.len());
        msg.extend_from_slice(PEER_LABEL);
        msg.extend_from_slice(peer_id.as_bytes());
        hmac(&self.mac_key, &msg)
    }

    fn store_key(&self) -> [u8; 32] {
        hmac(&self.mac_key, STORE_LABEL)
    }

    fn open<'a>(&self, payload: &'a [u8]) -> Result<&'a [u8], String> {
        let mac = payload.get(..MAC_LEN).ok_or("unsigned gossip datagram")?;
        let body = payload.get(MAC_LEN..).ok_or("truncated gossip datagram")?;
        let expected = hmac(&self.key_for(&local_node_id()), body);
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
        let expected = hmac(&self.store_key(), &caps_bytes);
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
        {
            let mac = hmac(&self.store_key(), &caps_bytes);
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

    /// Binds the gossip manager to a local UDP port.
    pub fn bind(&mut self, addr: &str) -> std::io::Result<()> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        self.socket = Some(Arc::new(socket));
        Ok(())
    }

    /// Bound socket address, if `bind` succeeded.
    pub fn local_addr(&self) -> Option<String> {
        self.socket
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

    /// One non-blocking recv. `Ok(true)` means a datagram was ingested.
    pub fn poll_recv(&self) -> std::io::Result<bool> {
        let Some(socket) = &self.socket else {
            return Ok(false);
        };
        let mut buf = [0u8; 2048];
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                let _ = self.ingest(&buf[..n], Some(&src.to_string()));
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Recv loop for the daemon process lifetime.
    pub fn spawn_recv_loop(&self) {
        let this = self.clone();
        let _ = std::thread::Builder::new()
            .name("susi-gossip-recv".into())
            .spawn(move || {
                loop {
                    match this.poll_recv() {
                        Ok(true) => {}
                        Ok(false) => std::thread::sleep(std::time::Duration::from_millis(50)),
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
                    }
                }
            });
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
    pub fn fanout_to_cluster(&self, gossip_port: u16) {
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
            if let Some(socket) = &self.socket {
                let msg = GossipMessage::PeerDiscovery {
                    from_ip: from_ip.clone(),
                    from_id: us.clone(),
                };
                if let Ok(body) = serde_json::to_vec(&msg) {
                    let payload = self.seal_for(&id, &body);
                    let _ = socket.send_to(&payload, &target);
                }
            }
        }
        self.persist();
    }

    /// Advertises this node's capabilities to a known peer.
    pub fn advertise(
        &self,
        target_addr: &str,
        peer_id: &str,
        cell_id: &str,
        capabilities: Vec<String>,
    ) -> std::io::Result<()> {
        if let Some(socket) = &self.socket {
            let msg = GossipMessage::AdvertiseCapabilities {
                cell_id: cell_id.to_string(),
                capabilities,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            };
            let body = serde_json::to_vec(&msg)?;
            let payload = self.seal_for(peer_id, &body);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gossip_capability_propagation() {
        let manager = GossipManager::new();

        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "remote-cell-1".to_string(),
            capabilities: vec!["infer".to_string(), "tool:git".to_string()],
            timestamp: 1600000000,
        };
        let body = serde_json::to_vec(&msg).unwrap();
        let payload = manager.seal(&body);

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
}
