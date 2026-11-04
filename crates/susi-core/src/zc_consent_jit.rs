//! Ask for egress consent at first cloud use (JIT).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressConsent {
    pub granted: bool,
}

impl EgressConsent {
    #[must_use]
    pub fn require_for_cloud(&self, wants_cloud: bool) -> bool {
        wants_cloud && !self.granted
    }

    pub fn grant(&mut self) {
        self.granted = true;
    }
}

#[cfg(test)]
mod zc_consent_jit_tests {
    use super::*;

    #[test]
    fn zc_consent_jit_asks_on_first_cloud() {
        let mut c = EgressConsent::default();
        assert!(c.require_for_cloud(true));
        assert!(!c.require_for_cloud(false));
        c.grant();
        assert!(!c.require_for_cloud(true));
    }

    #[test]
    fn egress_consent_default_is_ungranted_and_local_never_requires_consent() {
        assert!(!EgressConsent::default().granted);
        assert!(!EgressConsent { granted: true }.require_for_cloud(false));
    }

    #[test]
    fn egress_consent_grant_is_idempotent() {
        let mut c = EgressConsent::default();
        c.grant();
        c.grant();
        assert!(c.granted);
        assert!(!c.require_for_cloud(true));
    }

    #[test]
    fn egress_consent_serde_json_roundtrip() {
        let orig = EgressConsent { granted: true };
        let json = serde_json::to_string(&orig).expect("serialize");
        let back: EgressConsent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, orig);
    }

    #[test]
    fn egress_consent_clone_preserves_independent_grant_transitions() {
        let ungranted = EgressConsent::default();
        let mut cloned = ungranted.clone();
        assert_eq!(cloned, ungranted);
        cloned.grant();
        assert!(!ungranted.granted);
        assert!(cloned.granted);
        assert!(ungranted.require_for_cloud(true));
        assert!(!cloned.require_for_cloud(true));
    }
}
