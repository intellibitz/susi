//! Federation protocol compatibility negotiation (VC-201-034).
//!
//! Advertise supported wire and ledger versions with minimum security
//! requirements; accept compatible peers and reject insecure downgrades.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolOffer {
    pub wire_version: u32,
    pub ledger_version: u32,
    /// Minimum acceptable security level (higher = stricter).
    pub min_security: u32,
    pub peer_security: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NegotiateResult {
    Accept { wire: u32, ledger: u32 },
    Reject { reason: String },
}

/// Local capabilities we are willing to speak.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProtocol {
    pub wire_min: u32,
    pub wire_max: u32,
    pub ledger_min: u32,
    pub ledger_max: u32,
    pub min_security: u32,
}

impl LocalProtocol {
    #[must_use]
    pub fn negotiate(&self, peer: &ProtocolOffer) -> NegotiateResult {
        if peer.peer_security < self.min_security || peer.min_security > peer.peer_security {
            return NegotiateResult::Reject {
                reason: format!(
                    "insecure: peer security {} below required {}",
                    peer.peer_security, self.min_security
                ),
            };
        }
        if peer.wire_version < self.wire_min || peer.wire_version > self.wire_max {
            return NegotiateResult::Reject {
                reason: format!(
                    "unsupported wire version {} (local {}-{})",
                    peer.wire_version, self.wire_min, self.wire_max
                ),
            };
        }
        if peer.ledger_version < self.ledger_min || peer.ledger_version > self.ledger_max {
            return NegotiateResult::Reject {
                reason: format!(
                    "unsupported ledger version {} (local {}-{})",
                    peer.ledger_version, self.ledger_min, self.ledger_max
                ),
            };
        }
        // Reject explicit downgrade below our advertised minimums (already
        // covered by ranges) — also reject when peer asks us to drop security.
        if peer.min_security < self.min_security {
            return NegotiateResult::Reject {
                reason: format!(
                    "downgrade refused: peer min_security {} < local {}",
                    peer.min_security, self.min_security
                ),
            };
        }
        NegotiateResult::Accept {
            wire: peer.wire_version,
            ledger: peer.ledger_version,
        }
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    const LOCAL: LocalProtocol = LocalProtocol {
        wire_min: 1,
        wire_max: 3,
        ledger_min: 2,
        ledger_max: 4,
        min_security: 2,
    };

    #[test]
    fn negotiate_accepts_offer_within_wire_ledger_and_security_bounds() {
        let offer = ProtocolOffer {
            wire_version: 2,
            ledger_version: 3,
            min_security: 2,
            peer_security: 3,
        };
        assert_eq!(
            LOCAL.negotiate(&offer),
            NegotiateResult::Accept { wire: 2, ledger: 3 }
        );
    }

    #[test]
    fn negotiate_rejects_peer_with_insufficient_security_or_inconsistent_offer() {
        let low_peer = ProtocolOffer {
            wire_version: 2,
            ledger_version: 3,
            min_security: 2,
            peer_security: 1,
        };
        assert!(matches!(
            LOCAL.negotiate(&low_peer),
            NegotiateResult::Reject { reason } if reason.contains("insecure")
        ));

        let inconsistent = ProtocolOffer {
            wire_version: 2,
            ledger_version: 3,
            min_security: 4,
            peer_security: 3,
        };
        assert!(matches!(
            LOCAL.negotiate(&inconsistent),
            NegotiateResult::Reject { reason } if reason.contains("insecure")
        ));
    }

    #[test]
    fn negotiate_rejects_out_of_range_wire_or_ledger_versions() {
        let bad_wire = ProtocolOffer {
            wire_version: 9,
            ledger_version: 3,
            min_security: 2,
            peer_security: 2,
        };
        assert!(matches!(
            LOCAL.negotiate(&bad_wire),
            NegotiateResult::Reject { reason } if reason.contains("unsupported wire version")
        ));

        let bad_ledger = ProtocolOffer {
            wire_version: 2,
            ledger_version: 1,
            min_security: 2,
            peer_security: 2,
        };
        assert!(matches!(
            LOCAL.negotiate(&bad_ledger),
            NegotiateResult::Reject { reason } if reason.contains("unsupported ledger version")
        ));
    }

    #[test]
    fn negotiate_refuses_security_downgrade_when_peer_min_security_is_lower() {
        let downgrade = ProtocolOffer {
            wire_version: 2,
            ledger_version: 3,
            min_security: 1,
            peer_security: 2,
        };
        assert!(matches!(
            LOCAL.negotiate(&downgrade),
            NegotiateResult::Reject { reason } if reason.contains("downgrade refused")
        ));
    }

    #[test]
    fn negotiate_accepts_exact_boundary_wire_and_ledger_versions() {
        let min_bound = ProtocolOffer {
            wire_version: LOCAL.wire_min,
            ledger_version: LOCAL.ledger_min,
            min_security: LOCAL.min_security,
            peer_security: LOCAL.min_security,
        };
        assert_eq!(
            LOCAL.negotiate(&min_bound),
            NegotiateResult::Accept {
                wire: LOCAL.wire_min,
                ledger: LOCAL.ledger_min,
            }
        );

        let max_bound = ProtocolOffer {
            wire_version: LOCAL.wire_max,
            ledger_version: LOCAL.ledger_max,
            min_security: LOCAL.min_security,
            peer_security: LOCAL.min_security + 2,
        };
        assert_eq!(
            LOCAL.negotiate(&max_bound),
            NegotiateResult::Accept {
                wire: LOCAL.wire_max,
                ledger: LOCAL.ledger_max,
            }
        );
    }
}
