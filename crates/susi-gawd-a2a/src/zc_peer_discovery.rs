//! Discover LAN and localhost A2A peers with fingerprint confirmation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCandidate {
    pub url: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAdmission {
    pub admitted: bool,
    pub reason: String,
}

/// Admit a peer only after the human confirms the fingerprint.
#[must_use]
pub fn admit_peer(candidate: &PeerCandidate, confirmed_fingerprint: Option<&str>) -> PeerAdmission {
    match confirmed_fingerprint {
        Some(fp) if fp == candidate.fingerprint => PeerAdmission {
            admitted: true,
            reason: "fingerprint confirmed".into(),
        },
        Some(_) => PeerAdmission {
            admitted: false,
            reason: "fingerprint mismatch".into(),
        },
        None => PeerAdmission {
            admitted: false,
            reason: "awaiting fingerprint confirmation".into(),
        },
    }
}

#[cfg(test)]
mod zc_peer_discovery_tests {
    use super::*;

    #[test]
    fn zc_peer_discovery_requires_fingerprint_confirm() {
        let c = PeerCandidate {
            url: "http://127.0.0.1:9094".into(),
            fingerprint: "abc".into(),
        };
        assert!(!admit_peer(&c, None).admitted);
        assert!(admit_peer(&c, Some("abc")).admitted);
        assert!(!admit_peer(&c, Some("nope")).admitted);
    }
}
