//! Detect NAT / public reachability without env vars.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatStatus {
    pub public_reachable: bool,
    pub behind_nat: bool,
}

#[must_use]
pub fn detect_nat(local_ip_private: bool, stun_mapped: bool) -> NatStatus {
    NatStatus {
        behind_nat: local_ip_private,
        public_reachable: stun_mapped,
    }
}

#[cfg(test)]
mod zc_nat_auto_tests {
    use super::*;

    #[test]
    fn zc_nat_auto_detects_behind_nat() {
        let s = detect_nat(true, false);
        assert!(s.behind_nat);
        assert!(!s.public_reachable);
        let ok = detect_nat(true, true);
        assert!(ok.public_reachable);
    }
}
