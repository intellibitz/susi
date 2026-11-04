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

    #[test]
    fn detect_nat_public_ip_is_not_behind_nat() {
        let direct = detect_nat(false, true);
        assert!(!direct.behind_nat);
        assert!(direct.public_reachable);
    }

    #[test]
    fn detect_nat_unmapped_public_interface_reports_unreachable() {
        let offline = detect_nat(false, false);
        assert!(!offline.behind_nat);
        assert!(!offline.public_reachable);
    }

    #[test]
    fn nat_status_serde_json_roundtrip() {
        let status = detect_nat(true, true);
        let json = serde_json::to_string(&status).expect("serialize");
        let decoded: NatStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, status);
    }
}
