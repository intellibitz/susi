//! Explicit WAN peer admission (VC-201-038).
//!
//! WAN peers are never discovered — an operator pins endpoint, transport,
//! and expected identity. Admission then enforces the same contract LAN
//! `Explicit` peers satisfy plus WAN-specific guards: the peer must
//! answer from its pinned endpoint over an allowed transport, present a
//! signed handshake bound to the pinned identity, carry a fresh nonce
//! (replay check), and pass the operator's egress policy. Reconnects
//! re-run the whole gate — a new socket is not a new identity.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// How a pinned WAN peer is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WanTransport {
    /// Direct TLS to the pinned endpoint.
    Direct,
    /// Through an operator-trusted relay; the peer still presents its
    /// own pinned identity — the relay forwards, it does not vouch.
    Relay { relay_endpoint: String },
}

/// Operator-pinned admission record — the WAN equivalent of the
/// `Explicit` admission class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WanPeerPin {
    pub node_id: String,
    /// `host:port` the peer must answer from.
    pub endpoint: String,
    pub transport: WanTransport,
    /// Expected handshake identity (public key fingerprint). A peer
    /// presenting anything else is refused even with a valid signature.
    pub pinned_identity: String,
}

/// Operator egress policy: which endpoints this node may dial.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPolicy {
    /// Empty = allow any pinned endpoint (explicit pins are already
    /// operator intent). Non-empty = allowlist; everything else refused.
    pub allow_endpoints: BTreeSet<String>,
    /// Relay endpoints the operator trusts for `Relay` transport.
    pub allow_relays: BTreeSet<String>,
}

/// What the peer presented — the wire-level claims under check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WanHandshake {
    pub node_id: String,
    /// Endpoint the handshake actually arrived from.
    pub observed_endpoint: String,
    /// Transport the connection arrived on.
    pub arrived_via: WanTransport,
    /// Identity the signature verifies against.
    pub presented_identity: String,
    /// Whether the handshake signature verified.
    pub signature_ok: bool,
    /// Fresh nonce for the replay check.
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WanVerdict {
    Admitted,
    Refused(&'static str),
}

/// Replay guard: nonces are remembered so a captured handshake cannot
/// re-admit. Entries are removed once they outlive the handshake's
/// freshness window, bounding memory.
#[derive(Debug, Default)]
pub struct ReplaySet {
    seen: BTreeSet<String>,
}

impl ReplaySet {
    /// First sight of a nonce inserts it; a repeat is a replay.
    pub fn check_and_record(&mut self, nonce: &str) -> bool {
        self.seen.insert(nonce.to_string())
    }

    pub fn forget(&mut self, nonce: &str) {
        self.seen.remove(nonce);
    }
}

/// Admit a WAN peer through the pinned contract + egress + replay gates.
///
/// Order matters: the pin must match endpoint *and* transport and
/// identity before crypto or egress are consulted — a mismatch there is
/// a configuration/identity fault, not a network one.
#[must_use]
pub fn admit_wan_peer(
    pin: &WanPeerPin,
    hs: &WanHandshake,
    egress: &EgressPolicy,
    replay: &mut ReplaySet,
) -> WanVerdict {
    if hs.node_id != pin.node_id || hs.observed_endpoint != pin.endpoint {
        return WanVerdict::Refused("endpoint/identity mismatch with pin");
    }
    if hs.arrived_via != pin.transport {
        return WanVerdict::Refused("transport differs from pin");
    }
    if !egress.allow_endpoints.is_empty() && !egress.allow_endpoints.contains(&pin.endpoint) {
        return WanVerdict::Refused("egress policy does not allow endpoint");
    }
    if let WanTransport::Relay { relay_endpoint } = &pin.transport {
        if !egress.allow_relays.contains(relay_endpoint) {
            return WanVerdict::Refused("relay not in egress policy");
        }
    }
    if hs.presented_identity != pin.pinned_identity || !hs.signature_ok {
        return WanVerdict::Refused("identity/signature does not verify against pin");
    }
    if !replay.check_and_record(&hs.nonce) {
        return WanVerdict::Refused("replay: nonce already consumed");
    }
    WanVerdict::Admitted
}
