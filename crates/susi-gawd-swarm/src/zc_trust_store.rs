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
}
