//! Kademlia-style P2P Routing (Swarm OS Bullet 14)
//!
//! Routes by XOR distance between SHA-256 node-id digests, the same
//! metric Kademlia uses to rank which known peers are closest to a lookup
//! target.

use std::collections::HashMap;
use std::sync::RwLock;

use sha2::{Digest, Sha256};

fn digest(node_id: &str) -> [u8; 32] {
    Sha256::digest(node_id.as_bytes()).into()
}

fn xor_distance(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = a[i] ^ b[i];
    }
    out
}

pub struct P2pRouter {
    routing_table: RwLock<HashMap<String, String>>, // node_id -> multiaddr
}

impl Default for P2pRouter {
    fn default() -> Self {
        Self {
            routing_table: RwLock::new(HashMap::new()),
        }
    }
}

impl P2pRouter {
    pub fn add_peer(&self, node_id: &str, addr: &str) {
        self.routing_table
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node_id.to_string(), addr.to_string());
    }

    /// Returns up to `k` known peers ordered by XOR distance to
    /// `target_id`, nearest first — Kademlia's `FIND_NODE` response shape.
    pub fn closest_peers(&self, target_id: &str, k: usize) -> Vec<(String, String)> {
        let target = digest(target_id);
        let table = self.routing_table.read().unwrap_or_else(|e| e.into_inner());
        let mut ranked: Vec<(String, String, [u8; 32])> = table
            .iter()
            .map(|(id, addr)| (id.clone(), addr.clone(), xor_distance(&digest(id), &target)))
            .collect();
        ranked.sort_by_key(|entry| entry.2);
        ranked
            .into_iter()
            .take(k)
            .map(|(id, addr, _)| (id, addr))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_is_closest_with_zero_distance() {
        let router = P2pRouter::default();
        router.add_peer("node-a", "10.0.0.1:9000");
        router.add_peer("node-b", "10.0.0.2:9000");
        router.add_peer("node-c", "10.0.0.3:9000");

        let closest = router.closest_peers("node-b", 1);
        assert_eq!(
            closest,
            vec![("node-b".to_string(), "10.0.0.2:9000".to_string())]
        );
    }

    #[test]
    fn k_limits_the_result_size() {
        let router = P2pRouter::default();
        for i in 0..5 {
            router.add_peer(&format!("node-{i}"), &format!("10.0.0.{i}:9000"));
        }
        assert_eq!(router.closest_peers("node-2", 2).len(), 2);
    }
}
