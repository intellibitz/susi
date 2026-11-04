//! Bind address derived from peer consent.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindChoice {
    pub address: String,
    pub widened: bool,
}

/// Loopback until a consented LAN peer or remote client needs more.
#[must_use]
pub fn bind_address(consented_lan_peer: bool, remote_client_consented: bool) -> BindChoice {
    if consented_lan_peer || remote_client_consented {
        BindChoice {
            address: "0.0.0.0".into(),
            widened: true,
        }
    } else {
        BindChoice {
            address: "127.0.0.1".into(),
            widened: false,
        }
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod zc_bind_auto_tests {
    use super::*;

    #[test]
    fn zc_bind_auto_loopback_until_peer_consent() {
        let local = bind_address(false, false);
        assert_eq!(local.address, "127.0.0.1");
        assert!(!local.widened);
        let wide = bind_address(true, false);
        assert_eq!(wide.address, "0.0.0.0");
        assert!(wide.widened);
    }

    #[test]
    fn bind_address_widens_when_remote_client_consented_only() {
        let wide = bind_address(false, true);
        assert_eq!(wide.address, "0.0.0.0");
        assert!(wide.widened);
    }

    #[test]
    fn bind_address_widens_when_both_lan_and_remote_consented() {
        let wide = bind_address(true, true);
        assert_eq!(wide.address, "0.0.0.0");
        assert!(wide.widened);
    }

    #[test]
    fn bind_choice_serde_roundtrip_preserves_address_and_widened_flag() {
        let choice = bind_address(false, false);
        let json = serde_json::to_string(&choice).expect("serialize");
        let decoded: BindChoice = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, choice);
    }

    #[test]
    fn bind_choice_equality_distinguishes_loopback_from_widened_choice() {
        let local = bind_address(false, false);
        let wide = bind_address(true, false);
        assert_ne!(local, wide);
        assert_eq!(wide, bind_address(false, true));
    }
}
