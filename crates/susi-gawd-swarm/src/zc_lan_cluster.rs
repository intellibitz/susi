//! Cluster join on the LAN with mDNS and a confirmation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanPeer {
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinDecision {
    pub joined: bool,
    pub reason: String,
}

/// Join only after the operator confirms the discovered peer.
#[must_use]
pub fn confirm_join(peer: &LanPeer, confirmed: bool) -> JoinDecision {
    if confirmed {
        JoinDecision {
            joined: true,
            reason: format!("joined {}", peer.name),
        }
    } else {
        JoinDecision {
            joined: false,
            reason: "awaiting LAN join confirmation".into(),
        }
    }
}

#[cfg(test)]
mod zc_lan_cluster_tests {
    use super::*;

    #[test]
    fn zc_lan_cluster_requires_confirmation() {
        let p = LanPeer {
            name: "node-b".into(),
            address: "192.168.1.2".into(),
        };
        assert!(!confirm_join(&p, false).joined);
        assert!(confirm_join(&p, true).joined);
    }
}
