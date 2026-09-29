use crate::identity_revoke::{IdentityStore, KeyMaterial};
use std::collections::BTreeSet;

#[test]
fn vc_201_033_revoke_fails_after_restart_history_verifiable() {
    let mut s = IdentityStore::default();
    let electorate = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
    let key = KeyMaterial {
        peer_id: "p1".into(),
        key_id: "k1".into(),
        public: "pk1".into(),
    };
    s.rotate(
        "p1",
        key,
        &BTreeSet::from(["a".into(), "b".into()]),
        &electorate,
    )
    .unwrap();
    s.revoke("k1", &BTreeSet::from(["a".into(), "b".into()]), &electorate)
        .unwrap();
    // simulate restart: store still has revoked set
    assert!(!s.credential_ok_after_restart("k1"));
    assert!(s.historical_verifiable("k1"));
}

#[test]
fn vc_201_033_rotation_requires_quorum() {
    let mut s = IdentityStore::default();
    let electorate = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
    let err = s.rotate(
        "p1",
        KeyMaterial {
            peer_id: "p1".into(),
            key_id: "k2".into(),
            public: "pk2".into(),
        },
        &BTreeSet::from(["a".into()]),
        &electorate,
    );
    assert!(err.is_err());
}
