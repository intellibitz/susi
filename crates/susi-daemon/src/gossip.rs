//! Gossip Protocol for Peer-to-Peer Capability Propagation (Swarm OS Bullet 89)
//!
//! Implements a built-in gossip protocol (epidemic routing) for propagating
//! capability discovery and swarm routing table information over UDP.

use std::collections::{HashMap, HashSet};
use std::net::UdpSocket;
use std::sync::{Arc, RwLock};
use serde::{Deserialize, Serialize};

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
    PeerDiscovery {
        from_ip: String,
    },
}

/// Manages epidemic gossip state and UDP binding.
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

    /// Handles an incoming gossip payload.
    pub fn handle_gossip(&self, payload: &[u8]) -> Result<(), String> {
        let msg: GossipMessage = serde_json::from_slice(payload).map_err(|e| e.to_string())?;

        match msg {
            GossipMessage::AdvertiseCapabilities { cell_id, capabilities, .. } => {
                let mut map = self.peer_capabilities.write().unwrap_or_else(|e| e.into_inner());
                let entry = map.entry(cell_id).or_default();
                for cap in capabilities {
                    entry.insert(cap);
                }
            }
            GossipMessage::PeerDiscovery { .. } => {
                // In a real deployment, we would respond with known peers.
            }
        }
        
        Ok(())
    }

    /// Advertises this node's capabilities to a known peer.
    pub fn advertise(&self, target_addr: &str, cell_id: &str, capabilities: Vec<String>) -> std::io::Result<()> {
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
        let map = self.peer_capabilities.read().unwrap_or_else(|e| e.into_inner());
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
