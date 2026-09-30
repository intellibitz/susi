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
}
