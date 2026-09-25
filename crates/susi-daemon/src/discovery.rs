//! Local Peer Discovery Registry (Swarm OS Bullet 31)
//!
//! A lightweight stand-in for mDNS/SSDP-style discovery: peers announce
//! themselves with a TTL, and `sweep_expired` evicts anyone who hasn't
//! re-announced within it — the same liveness contract mDNS gives via
//! periodic re-broadcast, without requiring multicast sockets in-process.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

struct Announcement {
    addr: String,
    expires_at: Instant,
}

pub struct MdnsDiscovery {
    service_name: String,
    peers: RwLock<HashMap<String, Announcement>>,
}

impl MdnsDiscovery {
    pub fn new(service_name: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
            peers: RwLock::new(HashMap::new()),
        }
    }

    pub fn get_service_name(&self) -> &str {
        &self.service_name
    }

    /// Announces (or re-announces) `peer_id` at `addr`, valid for `ttl`.
    pub fn announce(&self, peer_id: &str, addr: &str, ttl: Duration) {
        let mut peers = self.peers.write().unwrap_or_else(|e| e.into_inner());
        peers.insert(
            peer_id.to_string(),
            Announcement {
                addr: addr.to_string(),
                expires_at: Instant::now() + ttl,
            },
        );
    }

    /// Evicts peers whose announcement has expired, returning how many were removed.
    pub fn sweep_expired(&self) -> usize {
        let mut peers = self.peers.write().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let before = peers.len();
        peers.retain(|_, ann| ann.expires_at > now);
        before - peers.len()
    }

    /// Lists currently live peers as `(peer_id, addr)`.
    pub fn active_peers(&self) -> Vec<(String, String)> {
        let peers = self.peers.read().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        peers
            .iter()
            .filter(|(_, ann)| ann.expires_at > now)
            .map(|(id, ann)| (id.clone(), ann.addr.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announced_peers_are_active_until_ttl_expires() {
        let disc = MdnsDiscovery::new("susi-gmcp._tcp");
        disc.announce("peer-a", "10.0.0.1:9000", Duration::from_millis(30));
        assert_eq!(
            disc.active_peers(),
            vec![("peer-a".to_string(), "10.0.0.1:9000".to_string())]
        );

        std::thread::sleep(Duration::from_millis(60));
        assert!(disc.active_peers().is_empty());
        assert_eq!(disc.sweep_expired(), 1);
    }

    #[test]
    fn re_announcing_refreshes_the_ttl() {
        let disc = MdnsDiscovery::new("susi-gmcp._tcp");
        disc.announce("peer-a", "10.0.0.1:9000", Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(10));
        disc.announce("peer-a", "10.0.0.1:9000", Duration::from_millis(200));
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(disc.active_peers().len(), 1);
    }
}
