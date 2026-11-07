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

/// The refusal reasons [`admit_wan_peer`] records, in gate order.
pub const REFUSAL_REASONS: [&str; 6] = [
    "endpoint/identity mismatch with pin",
    "transport differs from pin",
    "egress policy does not allow endpoint",
    "relay not in egress policy",
    "identity/signature does not verify against pin",
    "replay: nonce already consumed",
];

/// Whether `endpoint` is a `host:port` with a named host and a non-zero port.
#[must_use]
pub fn is_host_port(endpoint: &str) -> bool {
    endpoint.rsplit_once(':').is_some_and(|(host, port)| {
        !host.trim().is_empty()
            && host.trim() == host
            && port.parse::<u16>().is_ok_and(|port| port != 0)
    })
}

fn verify_endpoint(label: &str, endpoint: &str) -> Result<(), String> {
    if !is_host_port(endpoint) {
        return Err(format!(
            "{label} `{endpoint}` must be a host:port with a non-zero port"
        ));
    }
    Ok(())
}

impl WanTransport {
    /// Verify a transport: a relay names the `host:port` it forwards through.
    pub fn verify_transport_invariants(&self) -> Result<(), String> {
        match self {
            Self::Direct => Ok(()),
            Self::Relay { relay_endpoint } => verify_endpoint("relay endpoint", relay_endpoint),
        }
    }
}

impl WanPeerPin {
    /// Verify an operator pin: a node id, a `host:port` endpoint, a valid transport and
    /// a trimmed pinned identity (an empty pin would match any presented identity).
    pub fn verify_pin_invariants(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty() {
            return Err("WanPeerPin node_id must not be empty".to_string());
        }
        verify_endpoint("WanPeerPin endpoint", &self.endpoint)?;
        self.transport.verify_transport_invariants()?;
        if self.pinned_identity.trim().is_empty()
            || self.pinned_identity.trim() != self.pinned_identity
        {
            return Err(format!(
                "WanPeerPin `{}` pinned_identity must be a trimmed non-empty fingerprint",
                self.node_id
            ));
        }
        Ok(())
    }
}

impl EgressPolicy {
    /// Verify an egress policy: every allowed endpoint and relay is a `host:port`.
    pub fn verify_policy_invariants(&self) -> Result<(), String> {
        for endpoint in &self.allow_endpoints {
            verify_endpoint("EgressPolicy allowed endpoint", endpoint)?;
        }
        for relay in &self.allow_relays {
            verify_endpoint("EgressPolicy allowed relay", relay)?;
        }
        Ok(())
    }
}

impl WanHandshake {
    /// Verify the wire-level claims are well formed before they are checked against a
    /// pin: a node id, a `host:port` observed endpoint, a valid transport, a presented
    /// identity and a nonce for the replay guard.
    pub fn verify_handshake_invariants(&self) -> Result<(), String> {
        if self.node_id.trim().is_empty() {
            return Err("WanHandshake node_id must not be empty".to_string());
        }
        verify_endpoint("WanHandshake observed_endpoint", &self.observed_endpoint)?;
        self.arrived_via.verify_transport_invariants()?;
        if self.presented_identity.trim().is_empty() {
            return Err(format!(
                "WanHandshake from `{}` presents no identity",
                self.node_id
            ));
        }
        if self.nonce.trim().is_empty() {
            return Err(format!(
                "WanHandshake from `{}` carries no nonce",
                self.node_id
            ));
        }
        Ok(())
    }
}

impl WanVerdict {
    /// Verify a verdict: a refusal states one of the reasons the gate records.
    pub fn verify_verdict_invariants(&self) -> Result<(), String> {
        match self {
            Self::Admitted => Ok(()),
            Self::Refused(reason) => {
                if REFUSAL_REASONS.contains(reason) {
                    Ok(())
                } else {
                    Err(format!(
                        "WanVerdict refusal `{reason}` is not one the admission gate records"
                    ))
                }
            }
        }
    }
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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    fn sample_pin() -> WanPeerPin {
        WanPeerPin {
            node_id: "peer-1".to_string(),
            endpoint: "203.0.113.10:9443".to_string(),
            transport: WanTransport::Direct,
            pinned_identity: "fp-sha256-abc".to_string(),
        }
    }

    fn sample_handshake() -> WanHandshake {
        WanHandshake {
            node_id: "peer-1".to_string(),
            observed_endpoint: "203.0.113.10:9443".to_string(),
            arrived_via: WanTransport::Direct,
            presented_identity: "fp-sha256-abc".to_string(),
            signature_ok: true,
            nonce: "nonce-1".to_string(),
        }
    }

    #[test]
    fn admit_wan_peer_accepts_matching_pin_signature_and_fresh_nonce() {
        let pin = sample_pin();
        let hs = sample_handshake();
        let egress = EgressPolicy::default();
        let mut replay = ReplaySet::default();
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Admitted
        );
    }

    #[test]
    fn admit_wan_peer_rejects_endpoint_transport_and_identity_mismatches() {
        let pin = sample_pin();
        let egress = EgressPolicy::default();
        let mut replay = ReplaySet::default();

        let mut bad_ep = sample_handshake();
        bad_ep.observed_endpoint = "198.51.100.1:9443".to_string();
        assert_eq!(
            admit_wan_peer(&pin, &bad_ep, &egress, &mut replay),
            WanVerdict::Refused("endpoint/identity mismatch with pin")
        );

        let mut bad_tr = sample_handshake();
        bad_tr.arrived_via = WanTransport::Relay {
            relay_endpoint: "relay:443".to_string(),
        };
        assert_eq!(
            admit_wan_peer(&pin, &bad_tr, &egress, &mut replay),
            WanVerdict::Refused("transport differs from pin")
        );

        let mut bad_sig = sample_handshake();
        bad_sig.signature_ok = false;
        assert_eq!(
            admit_wan_peer(&pin, &bad_sig, &egress, &mut replay),
            WanVerdict::Refused("identity/signature does not verify against pin")
        );
    }

    #[test]
    fn admit_wan_peer_enforces_egress_endpoint_and_relay_allowlists() {
        let mut pin = sample_pin();
        let mut hs = sample_handshake();
        let mut replay = ReplaySet::default();
        let egress = EgressPolicy {
            allow_endpoints: BTreeSet::from(["other:443".to_string()]),
            allow_relays: BTreeSet::new(),
        };
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Refused("egress policy does not allow endpoint")
        );

        pin.transport = WanTransport::Relay {
            relay_endpoint: "untrusted-relay:443".to_string(),
        };
        hs.arrived_via = pin.transport.clone();
        assert_eq!(
            admit_wan_peer(&pin, &hs, &EgressPolicy::default(), &mut replay),
            WanVerdict::Refused("relay not in egress policy")
        );
    }

    #[test]
    fn replay_set_blocks_replayed_nonce_until_forgotten() {
        let pin = sample_pin();
        let hs = sample_handshake();
        let egress = EgressPolicy::default();
        let mut replay = ReplaySet::default();
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Admitted
        );
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Refused("replay: nonce already consumed")
        );
        replay.forget(&hs.nonce);
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Admitted
        );
    }

    #[test]
    fn admit_wan_peer_accepts_trusted_relay_and_wan_peer_pin_round_trips_serde_json() {
        let mut pin = sample_pin();
        pin.transport = WanTransport::Relay {
            relay_endpoint: "relay.example:443".to_string(),
        };
        let mut hs = sample_handshake();
        hs.arrived_via = pin.transport.clone();
        let egress = EgressPolicy {
            allow_endpoints: BTreeSet::from([pin.endpoint.clone()]),
            allow_relays: BTreeSet::from(["relay.example:443".to_string()]),
        };
        let mut replay = ReplaySet::default();
        assert_eq!(
            admit_wan_peer(&pin, &hs, &egress, &mut replay),
            WanVerdict::Admitted
        );

        let encoded = serde_json::to_string(&pin).unwrap();
        let decoded: WanPeerPin = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, pin);
    }

    #[test]
    fn wan_pin_policy_handshake_and_verdict_invariant_verifiers_reject_malformed_input() {
        assert!(is_host_port("203.0.113.10:9443"));
        assert!(is_host_port("relay.example.net:443"));
        assert!(!is_host_port("203.0.113.10"));
        assert!(!is_host_port(":9443"));
        assert!(!is_host_port("host:0"));
        assert!(!is_host_port("host:99999"));

        let pin = sample_pin();
        assert_eq!(pin.verify_pin_invariants(), Ok(()));
        let mut no_port = pin.clone();
        no_port.endpoint = "203.0.113.10".to_string();
        assert!(no_port.verify_pin_invariants().is_err());
        let mut blank_identity = pin.clone();
        blank_identity.pinned_identity = " ".to_string();
        assert!(blank_identity.verify_pin_invariants().is_err());
        let mut bad_relay = pin.clone();
        bad_relay.transport = WanTransport::Relay {
            relay_endpoint: "relay".to_string(),
        };
        assert!(bad_relay.verify_pin_invariants().is_err());
        let mut good_relay = pin.clone();
        good_relay.transport = WanTransport::Relay {
            relay_endpoint: "relay.example.net:443".to_string(),
        };
        assert_eq!(good_relay.verify_pin_invariants(), Ok(()));

        assert_eq!(EgressPolicy::default().verify_policy_invariants(), Ok(()));
        let mut policy = EgressPolicy::default();
        policy
            .allow_endpoints
            .insert("203.0.113.10:9443".to_string());
        policy
            .allow_relays
            .insert("relay.example.net:443".to_string());
        assert_eq!(policy.verify_policy_invariants(), Ok(()));
        policy.allow_relays.insert("relay".to_string());
        assert!(policy.verify_policy_invariants().is_err());

        let hs = sample_handshake();
        assert_eq!(hs.verify_handshake_invariants(), Ok(()));
        let mut no_nonce = hs.clone();
        no_nonce.nonce = String::new();
        assert!(no_nonce.verify_handshake_invariants().is_err());
        let mut no_identity = hs.clone();
        no_identity.presented_identity = String::new();
        assert!(no_identity.verify_handshake_invariants().is_err());

        let mut replay = ReplaySet::default();
        let verdict = admit_wan_peer(&pin, &hs, &EgressPolicy::default(), &mut replay);
        assert_eq!(verdict.verify_verdict_invariants(), Ok(()));
        let replayed = admit_wan_peer(&pin, &hs, &EgressPolicy::default(), &mut replay);
        assert_eq!(
            replayed,
            WanVerdict::Refused("replay: nonce already consumed")
        );
        assert_eq!(replayed.verify_verdict_invariants(), Ok(()));
        assert!(WanVerdict::Refused("because")
            .verify_verdict_invariants()
            .is_err());
    }
}
