//! Renew consent tokens without a manual command.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentToken {
    pub expires_unix: u64,
    pub renewed: bool,
}

impl ConsentToken {
    pub fn renew_if_needed(&mut self, now_unix: u64, ttl_secs: u64) -> bool {
        if now_unix < self.expires_unix {
            return false;
        }
        self.expires_unix = now_unix.saturating_add(ttl_secs);
        self.renewed = true;
        true
    }
}

#[cfg(test)]
mod zc_consent_renewal_tests {
    use super::*;

    #[test]
    fn zc_consent_renewal_extends_expired_token() {
        let mut t = ConsentToken {
            expires_unix: 10,
            renewed: false,
        };
        assert!(!t.renew_if_needed(5, 100));
        assert!(t.renew_if_needed(11, 100));
        assert_eq!(t.expires_unix, 111);
        assert!(t.renewed);
    }
}
