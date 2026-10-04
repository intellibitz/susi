use crate::memory_replicate::{converge, MemoryReplica, ScopedRecord};

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
