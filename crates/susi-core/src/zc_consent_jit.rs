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
}
