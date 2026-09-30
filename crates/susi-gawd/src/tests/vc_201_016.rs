use crate::reflex_revisions::{ReflexRevision, ReflexStore};

#[test]
fn vc_201_016_pin_survives_revoke_for_caller() {
    let mut s = ReflexStore::default();
    s.publish(ReflexRevision {
        id: "v1".into(),
        source_hash: "s1".into(),
        wasm_digest: "w1".into(),
        grants: vec!["tool:list".into()],
        revoked: false,
    });
    s.pin("caller-a", "v1").unwrap();
    s.revoke("v1").unwrap();
    assert!(s.may_execute("caller-a", "v1"));
    assert!(!s.may_execute("caller-b", "v1"));
}

#[test]
fn vc_201_016_rollback_restores_pin() {
    let mut s = ReflexStore::default();
    for id in ["v1", "v2"] {
        s.publish(ReflexRevision {
            id: id.into(),
            source_hash: id.into(),
            wasm_digest: id.into(),
            grants: vec![],
            revoked: false,
        });
    }
    s.pin("c", "v2").unwrap();
    s.rollback_pin("c", "v1").unwrap();
    assert_eq!(s.pins.get("c").map(String::as_str), Some("v1"));
}
