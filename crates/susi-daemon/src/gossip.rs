//! Gossip Protocol for Peer-to-Peer Capability Propagation
//!
//! Implements a built-in gossip protocol (epidemic routing) for propagating
//! capability discovery and swarm routing table information over UDP.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::UdpSocket;
use std::sync::{Arc, RwLock};

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
    PeerDiscovery { from_ip: String },
}

/// Manages epidemic gossip state and UDP binding.
#[derive(Clone)]
pub struct GossipManager {
    /// Maps cell_id to a list of known capabilities.
    pub peer_capabilities: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// UDP socket for broadcasting / receiving.
    socket: Option<Arc<UdpSocket>>,
}

impl Default for GossipManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GossipManager {
    pub fn new() -> Self {
        Self {
            peer_capabilities: Arc::new(RwLock::new(HashMap::new())),
            socket: None,
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

    /// Handles an incoming gossip payload.
    pub fn handle_gossip(&self, payload: &[u8]) -> Result<(), String> {
        self.ingest(payload, None)
    }

    /// Ingest a payload, optionally advertising back to `from` on discovery.
    pub fn ingest(&self, payload: &[u8], from: Option<&str>) -> Result<(), String> {
        let msg: GossipMessage = serde_json::from_slice(payload).map_err(|e| e.to_string())?;

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
            }
            GossipMessage::PeerDiscovery { .. } => {
                if let Some(addr) = from {
                    let _ = self.advertise(
                        addr,
                        "susi-host",
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
        for (_id, addr) in peers {
            let target = Self::gossip_target(&addr, gossip_port);
            let _ = self.advertise(
                &target,
                "susi-host",
                vec!["host".to_string(), "infer".to_string()],
            );
            if let Some(socket) = &self.socket {
                let msg = GossipMessage::PeerDiscovery {
                    from_ip: from_ip.clone(),
                };
                if let Ok(payload) = serde_json::to_vec(&msg) {
                    let _ = socket.send_to(&payload, &target);
                }
            }
        }
    }

    /// Advertises this node's capabilities to a known peer.
    pub fn advertise(
        &self,
        target_addr: &str,
        cell_id: &str,
        capabilities: Vec<String>,
    ) -> std::io::Result<()> {
        if let Some(socket) = &self.socket {
            let msg = GossipMessage::AdvertiseCapabilities {
                cell_id: cell_id.to_string(),
                capabilities,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            let payload = serde_json::to_vec(&msg)?;
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

        // Simulate receiving a gossip packet
        let msg = GossipMessage::AdvertiseCapabilities {
            cell_id: "remote-cell-1".to_string(),
            capabilities: vec!["infer".to_string(), "tool:git".to_string()],
            timestamp: 1600000000,
        };
        let payload = serde_json::to_vec(&msg).unwrap();

        manager.handle_gossip(&payload).unwrap();

        // Verify capability is recorded
        assert!(manager.peer_has_capability("remote-cell-1", "infer"));
        assert!(manager.peer_has_capability("remote-cell-1", "tool:git"));
        assert!(!manager.peer_has_capability("remote-cell-1", "audit:seal"));
    }
}
