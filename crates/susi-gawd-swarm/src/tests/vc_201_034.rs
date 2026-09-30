use crate::federation_compat::{LocalProtocol, NegotiateResult, ProtocolOffer};

#[test]
fn vc_201_034_accepts_compatible_peer() {
    let local = LocalProtocol {
        wire_min: 1,
        wire_max: 3,
        ledger_min: 1,
        ledger_max: 2,
        min_security: 2,
    };
    let peer = ProtocolOffer {
        wire_version: 2,
        ledger_version: 2,
        min_security: 2,
        peer_security: 3,
    };
    assert_eq!(
        local.negotiate(&peer),
        NegotiateResult::Accept { wire: 2, ledger: 2 }
    );
}

#[test]
fn vc_201_034_rejects_unsupported_and_insecure_downgrade() {
    let local = LocalProtocol {
        wire_min: 2,
        wire_max: 3,
        ledger_min: 1,
        ledger_max: 2,
        min_security: 3,
    };
    let old_wire = ProtocolOffer {
        wire_version: 1,
        ledger_version: 1,
        min_security: 3,
        peer_security: 3,
    };
    match local.negotiate(&old_wire) {
        NegotiateResult::Reject { reason } => assert!(reason.contains("wire")),
        other => panic!("expected reject, got {other:?}"),
    }
    let insecure = ProtocolOffer {
        wire_version: 2,
        ledger_version: 1,
        min_security: 1,
        peer_security: 1,
    };
    match local.negotiate(&insecure) {
        NegotiateResult::Reject { reason } => {
            assert!(reason.contains("insecure") || reason.contains("downgrade"));
        }
        other => panic!("expected reject, got {other:?}"),
    }
}
