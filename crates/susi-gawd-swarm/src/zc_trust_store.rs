//! Swarm trust store with defaults.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustStore {
    pub trusted_peers: BTreeSet<String>,
}

impl TrustStore {
    pub fn trust(&mut self, peer: &str) {
        self.trusted_peers.insert(peer.to_string());
    }

    #[must_use]
    pub fn is_trusted(&self, peer: &str) -> bool {
        self.trusted_peers.contains(peer)
    }
}

#[cfg(test)]
mod zc_trust_store_tests {
    use super::*;

    #[test]
    fn zc_trust_store_records_peers() {
        let mut s = TrustStore::default();
        assert!(!s.is_trusted("p1"));
        s.trust("p1");
        assert!(s.is_trusted("p1"));
    }

    #[test]
    fn trust_store_default_is_empty_and_rejects_unknown_peers() {
        let s = TrustStore::default();
        assert!(s.trusted_peers.is_empty());
        assert!(!s.is_trusted("unknown-peer"));
    }

    #[test]
    fn trust_is_idempotent_and_isolates_distinct_peer_ids() {
        let mut s = TrustStore::default();
        s.trust("peer-a");
        s.trust("peer-a");
        s.trust("peer-b");
        assert_eq!(s.trusted_peers.len(), 2);
        assert!(s.is_trusted("peer-a"));
        assert!(s.is_trusted("peer-b"));
        assert!(!s.is_trusted("peer-c"));
    }

    #[test]
    fn trust_store_serde_json_roundtrip_preserves_peer_set() {
        let mut s = TrustStore::default();
        s.trust("node-1");
        s.trust("node-2");
        let json = serde_json::to_string(&s).expect("serialize");
        let back: TrustStore = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn trust_store_clone_and_debug_preserve_sorted_trusted_peers() {
        let mut s = TrustStore::default();
        s.trust("z-peer");
        s.trust("a-peer");
        s.trust("m-peer");
        let ordered: Vec<&str> = s.trusted_peers.iter().map(String::as_str).collect();
        assert_eq!(ordered, vec!["a-peer", "m-peer", "z-peer"]);
        assert_eq!(s.clone(), s);
        assert!(format!("{s:?}").contains("a-peer"));
    }
}
