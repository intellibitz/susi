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
