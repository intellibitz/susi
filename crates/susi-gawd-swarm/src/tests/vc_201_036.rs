use crate::snapshot_bootstrap::{bootstrap, sign, BootstrapResult, SnapshotManifest, TrustAnchor};
use std::collections::BTreeSet;

#[test]
fn vc_201_036_accepts_signed_snapshot() {
    let roster = BTreeSet::from(["a".into(), "b".into()]);
    let history = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
    let sig = sign("a", "snap-1", "tail-9", &roster);
    let m = SnapshotManifest {
        snapshot_id: "snap-1".into(),
        ledger_tail: "tail-9".into(),
        roster: roster.clone(),
        signature: sig,
        signer: "a".into(),
    };
    let anchors = vec![TrustAnchor {
        peer_id: "a".into(),
        public_key: "pk-a".into(),
    }];
    assert_eq!(bootstrap(&m, &anchors, &history), BootstrapResult::Ok);
}

#[test]
fn vc_201_036_rejects_tampered_missing_unauthorized() {
    let roster = BTreeSet::from(["a".into()]);
    let history = BTreeSet::from(["a".into()]);
    let anchors = vec![TrustAnchor {
        peer_id: "a".into(),
        public_key: "pk".into(),
    }];
    let tampered = SnapshotManifest {
        snapshot_id: "s".into(),
        ledger_tail: "t".into(),
        roster: roster.clone(),
        signature: "bad".into(),
        signer: "a".into(),
    };
    assert_eq!(
        bootstrap(&tampered, &anchors, &history),
        BootstrapResult::Tampered
    );
    let good_sig = sign("a", "s", "t", &roster);
    let missing = SnapshotManifest {
        snapshot_id: "s".into(),
        ledger_tail: "t".into(),
        roster: roster.clone(),
        signature: good_sig.clone(),
        signer: "ghost".into(),
    };
    assert_eq!(
        bootstrap(&missing, &anchors, &history),
        BootstrapResult::MissingAnchor
    );
    let bad_roster = BTreeSet::from(["a".into(), "evil".into()]);
    let unauthorized = SnapshotManifest {
        snapshot_id: "s".into(),
        ledger_tail: "t".into(),
        roster: bad_roster.clone(),
        signature: sign("a", "s", "t", &bad_roster),
        signer: "a".into(),
    };
    assert_eq!(
        bootstrap(&unauthorized, &anchors, &history),
        BootstrapResult::UnauthorizedRoster
    );
}
