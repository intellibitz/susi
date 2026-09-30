use crate::wan_peers::{
    admit_wan_peer, EgressPolicy, ReplaySet, WanHandshake, WanPeerPin, WanTransport, WanVerdict,
};
use std::collections::BTreeSet;

fn pin() -> WanPeerPin {
    WanPeerPin {
        node_id: "peer-eu".into(),
        endpoint: "10.9.8.7:9190".into(),
        transport: WanTransport::Direct,
        pinned_identity: "fingerprint-eu".into(),
    }
}

fn good_hs() -> WanHandshake {
    WanHandshake {
        node_id: "peer-eu".into(),
        observed_endpoint: "10.9.8.7:9190".into(),
        arrived_via: WanTransport::Direct,
        presented_identity: "fingerprint-eu".into(),
        signature_ok: true,
        nonce: "n-1".into(),
    }
}

#[test]
fn vc_201_038_pinned_direct_peer_admits_and_reconnects() {
    let mut replay = ReplaySet::default();
    let egress = EgressPolicy::default();
    assert_eq!(
        admit_wan_peer(&pin(), &good_hs(), &egress, &mut replay),
        WanVerdict::Admitted
    );
    // Reconnect with a fresh nonce: same pin admits again — socket
    // identity is not peer identity.
    let hs = WanHandshake {
        nonce: "n-2".into(),
        ..good_hs()
    };
    assert_eq!(
        admit_wan_peer(&pin(), &hs, &egress, &mut replay),
        WanVerdict::Admitted
    );
}

#[test]
fn vc_201_038_replay_nonce_is_refused() {
    let mut replay = ReplaySet::default();
    let egress = EgressPolicy::default();
    let first = admit_wan_peer(&pin(), &good_hs(), &egress, &mut replay);
    assert_eq!(first, WanVerdict::Admitted);
    assert_eq!(
        admit_wan_peer(&pin(), &good_hs(), &egress, &mut replay),
        WanVerdict::Refused("replay: nonce already consumed")
    );
}

#[test]
fn vc_201_038_wrong_endpoint_transport_or_identity_is_refused() {
    let egress = EgressPolicy::default();
    let mut replay = ReplaySet::default();
    // Answered from a different address than pinned.
    let hs = WanHandshake {
        observed_endpoint: "10.9.8.8:9190".into(),
        ..good_hs()
    };
    assert!(matches!(
        admit_wan_peer(&pin(), &hs, &egress, &mut replay),
        WanVerdict::Refused(_)
    ));
    // Arrived via relay when pin says direct.
    let hs = WanHandshake {
        arrived_via: WanTransport::Relay {
            relay_endpoint: "r".into(),
        },
        ..good_hs()
    };
    assert!(matches!(
        admit_wan_peer(&pin(), &hs, &egress, &mut replay),
        WanVerdict::Refused("transport differs from pin")
    ));
    // Valid signature but wrong identity — confidentiality check.
    let hs = WanHandshake {
        presented_identity: "fingerprint-attacker".into(),
        ..good_hs()
    };
    assert!(matches!(
        admit_wan_peer(&pin(), &hs, &egress, &mut replay),
        WanVerdict::Refused("identity/signature does not verify against pin")
    ));
    // Bad signature on the right identity.
    let hs = WanHandshake {
        signature_ok: false,
        ..good_hs()
    };
    assert!(matches!(
        admit_wan_peer(&pin(), &hs, &egress, &mut replay),
        WanVerdict::Refused(_)
    ));
}

#[test]
fn vc_201_038_egress_policy_bounds_pinned_endpoints_and_relays() {
    let mut replay = ReplaySet::default();
    // Allowlist present but the pinned endpoint is not on it.
    let egress = EgressPolicy {
        allow_endpoints: BTreeSet::from(["10.1.1.1:9190".to_string()]),
        ..EgressPolicy::default()
    };
    assert_eq!(
        admit_wan_peer(&pin(), &good_hs(), &egress, &mut replay),
        WanVerdict::Refused("egress policy does not allow endpoint")
    );

    // Relay transport: relay must be explicitly trusted.
    let relay_pin = WanPeerPin {
        transport: WanTransport::Relay {
            relay_endpoint: "relay.example:443".into(),
        },
        ..pin()
    };
    let hs = WanHandshake {
        arrived_via: WanTransport::Relay {
            relay_endpoint: "relay.example:443".into(),
        },
        ..good_hs()
    };
    let egress = EgressPolicy {
        allow_endpoints: BTreeSet::from(["10.9.8.7:9190".to_string()]),
        allow_relays: BTreeSet::new(),
    };
    assert_eq!(
        admit_wan_peer(&relay_pin, &hs, &egress, &mut replay),
        WanVerdict::Refused("relay not in egress policy")
    );
    let egress = EgressPolicy {
        allow_relays: BTreeSet::from(["relay.example:443".to_string()]),
        ..egress
    };
    assert_eq!(
        admit_wan_peer(&relay_pin, &hs, &egress, &mut replay),
        WanVerdict::Admitted
    );
}
