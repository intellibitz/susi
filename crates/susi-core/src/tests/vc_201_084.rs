use crate::memory_replicate::{
    compare_provenance, converge, MemoryReplica, PeerAuth, ScopedRecord,
};

#[test]
fn vc_201_084_skips_local_only_and_tombstones() {
    let mut a = MemoryReplica::new();
    a.upsert(ScopedRecord {
        id: "shared".into(),
        body: "x".into(),
        provenance: "p1".into(),
        local_only: false,
        deleted: false,
    });
    a.upsert(ScopedRecord {
        id: "secret".into(),
        body: "local".into(),
        provenance: "p1".into(),
        local_only: true,
        deleted: false,
    });
    a.delete("gone");
    a.upsert(ScopedRecord {
        id: "gone".into(),
        body: "revive".into(),
        provenance: "p2".into(),
        local_only: false,
        deleted: false,
    });
    let export = a.export_for_peers();
    assert!(export
        .iter()
        .filter(|r| !r.deleted)
        .all(|r| r.id == "shared"));
    assert!(!a.records.contains_key("gone"));
    assert!(export.iter().any(|r| r.id == "gone" && r.deleted));
}

#[test]
fn vc_201_084_disconnected_replicas_converge() {
    let mut a = MemoryReplica::new();
    let mut b = MemoryReplica::new();
    a.upsert(ScopedRecord {
        id: "r1".into(),
        body: "from-a".into(),
        provenance: "a".into(),
        local_only: false,
        deleted: false,
    });
    b.upsert(ScopedRecord {
        id: "r2".into(),
        body: "from-b".into(),
        provenance: "b".into(),
        local_only: false,
        deleted: false,
    });
    b.upsert(ScopedRecord {
        id: "local-b".into(),
        body: "nope".into(),
        provenance: "b".into(),
        local_only: true,
        deleted: false,
    });
    a.delete("r2"); // tombstone on a before sync
    let (a2, b2) = converge(&a, &b);
    assert!(a2.records.contains_key("r1"));
    assert!(!a2.records.contains_key("r2"));
    assert!(!b2.records.contains_key("r2"));
    assert!(b2.records.contains_key("r1"));
    assert!(!a2.records.contains_key("local-b"));
}

#[test]
fn vc_201_084_natural_provenance_ordering_prefers_p10_over_p9_on_merge() {
    assert_eq!(compare_provenance("p10", "p9"), std::cmp::Ordering::Greater);
    let mut replica = MemoryReplica::new();
    replica.upsert(ScopedRecord {
        id: "fact-num".into(),
        body: "v-ten".into(),
        provenance: "p10".into(),
        local_only: false,
        deleted: false,
    });
    replica.merge_from_peer(&[ScopedRecord {
        id: "fact-num".into(),
        body: "v-nine".into(),
        provenance: "p9".into(),
        local_only: false,
        deleted: false,
    }]);
    let rec = replica.records.get("fact-num").unwrap();
    assert_eq!(rec.body, "v-ten");
    assert_eq!(rec.provenance, "p10");
}

#[test]
fn vc_201_084_authenticated_peer_export_and_merge_enforce_peer_token() {
    let mut r = MemoryReplica::new().with_peer_auth("shared-secret");
    r.upsert(ScopedRecord {
        id: "k1".into(),
        body: "v1".into(),
        provenance: "p1".into(),
        local_only: false,
        deleted: false,
    });
    assert_eq!(
        r.export_for_peer_authenticated(&PeerAuth::new("peer-a", "wrong-token")),
        Err("peer authentication failed")
    );
    assert_eq!(
        r.merge_from_peer_authenticated(&[], &PeerAuth::new("", "shared-secret")),
        Err("peer authentication failed")
    );
    let exported = r
        .export_for_peer_authenticated(&PeerAuth::new("peer-a", "shared-secret"))
        .unwrap();
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].id, "k1");
}
