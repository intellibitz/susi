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

use crate::node_enrollment::enroll;

#[test]
fn vc_201_033_enrollment_requires_token_and_mtls() {
    assert!(!enroll("x", "secret", Some("node-a"), 1).accepted);
    assert!(!enroll("secret", "secret", None, 1).accepted);
    let ok = enroll("secret", "secret", Some("node-a"), 1);
    assert!(ok.accepted);
    assert_eq!(ok.peer_id.as_deref(), Some("node-a"));
    assert_eq!(ok.key_epoch, Some(1));
}

/// Production enrollment path: valid production token plus mTLS identity
/// provisions peer credentials and the cluster key epoch.
#[test]
fn node_enrollment_production() {
    let mut dag = crate::dag::MissionDag::new("enroll production");
    let prod = dag.enroll_node(
        "prod-token-2026",
        "prod-token-2026",
        Some("node-prod-1"),
        42,
    );
    assert!(prod.accepted);
    assert_eq!(prod.reason, "enrolled");
    assert_eq!(prod.peer_id.as_deref(), Some("node-prod-1"));
    assert_eq!(prod.key_epoch, Some(42));
    assert!(dag.roster.0.contains("node-prod-1"));

    // Wrong token rejected even with mTLS — roster unchanged.
    let before = dag.roster.clone();
    let bad = dag.enroll_node("wrong", "prod-token-2026", Some("node-prod-1"), 42);
    assert!(!bad.accepted);
    assert!(bad.peer_id.is_none());
    assert_eq!(dag.roster, before);

    // No mTLS rejected even with correct token.
    let no_tls = dag.enroll_node("prod-token-2026", "prod-token-2026", None, 42);
    assert!(!no_tls.accepted);
    assert!(no_tls.key_epoch.is_none());
}
