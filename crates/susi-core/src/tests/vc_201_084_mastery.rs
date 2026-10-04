//! Mastery verification for VC-201-084: replicate only policy-eligible
//! swarm memory over authenticated peers.
//!
//! The cited tests cover the happy path inside one helper. The
//! distinguishing properties are correct conflict resolution (a newer
//! write must win), deletion propagation on the documented export/merge
//! path, and peer authentication.

use crate::memory_replicate::{converge, MemoryReplica, ScopedRecord};

fn rec(id: &str, body: &str, prov: &str, local: bool, deleted: bool) -> ScopedRecord {
    ScopedRecord {
        id: id.into(),
        body: body.into(),
        provenance: prov.into(),
        local_only: local,
        deleted,
    }
}

/// Mastery: conflict resolution compares provenance using natural/numeric ordering.
/// Newer writes ("p10") outrank older writes ("p9") so the replica keeps the newer record.
#[test]
fn vc_201_084_mastery_numeric_provenance_newer_write_wins() {
    let mut r = MemoryReplica::new();
    r.upsert(rec("m1", "old", "p9", false, false));
    // A peer's newer write (provenance p10) outranks p9.
    r.merge_from_peer(&[rec("m1", "new", "p10", false, false)]);
    assert_eq!(
        r.records["m1"].body, "new",
        "the newer write must win via natural provenance comparison"
    );
}

/// Mastery: deletion propagates on the export/merge path.
/// export_for_peers carries tombstones as deleted records, and merge_from_peer
/// records the tombstone and removes the deleted key.
#[test]
fn vc_201_084_mastery_delete_propagates_via_export_merge() {
    let mut a = MemoryReplica::new();
    let mut b = MemoryReplica::new();
    a.upsert(rec("x", "shared", "p1", false, false));
    b.merge_from_peer(&a.export_for_peers());
    assert!(b.records.contains_key("x"));

    a.delete("x");
    // The documented replication path carries the tombstone.
    b.merge_from_peer(&a.export_for_peers());
    assert!(
        !b.records.contains_key("x"),
        "deleted record must not survive on the peer"
    );
    assert!(
        b.tombstones.contains("x"),
        "peer must record tombstone after merging export"
    );
}

/// Mastery: peer authentication is enforced on export and merge when configured.
#[test]
fn vc_201_084_mastery_peer_auth_enforced() {
    use crate::memory_replicate::PeerAuth;

    let mut a = MemoryReplica::new().with_peer_auth("secret-token");
    a.upsert(rec("item1", "data", "p1", false, false));

    let bad_auth = PeerAuth::new("peer-b", "wrong-token");
    assert!(a.export_for_peer_authenticated(&bad_auth).is_err());
    assert!(a
        .merge_from_peer_authenticated(&[rec("item2", "data2", "p1", false, false)], &bad_auth)
        .is_err());

    let good_auth = PeerAuth::new("peer-b", "secret-token");
    let export = a
        .export_for_peer_authenticated(&good_auth)
        .expect("auth should succeed");
    assert!(export.iter().any(|r| r.id == "item1"));

    let res =
        a.merge_from_peer_authenticated(&[rec("item2", "data2", "p2", false, false)], &good_auth);
    assert!(res.is_ok());
    assert!(a.records.contains_key("item2"));
}

/// What does hold: the combined converge() helper syncs tombstones both
/// ways, local_only records never leave a replica, and tombstoned records
/// are never revived.
#[test]
fn vc_201_084_mastery_converge_and_eligibility_hold() {
    let mut a = MemoryReplica::new();
    let mut b = MemoryReplica::new();
    a.upsert(rec("shared", "x", "p1", false, false));
    a.upsert(rec("mine", "local", "p1", true, false));
    a.delete("gone");
    b.upsert(rec("theirs", "y", "p2", false, false));

    let (a2, b2) = converge(&a, &b);
    assert!(a2.records.contains_key("theirs"));
    assert!(b2.records.contains_key("shared"));
    assert!(
        !b2.records.contains_key("mine"),
        "local_only never exported"
    );
    assert!(a2.tombstones.contains("gone") && b2.tombstones.contains("gone"));
    // A revive attempt stays dead on both sides.
    let mut a3 = a2;
    a3.upsert(rec("gone", "revive", "p9", false, false));
    assert!(!a3.records.contains_key("gone"));
}
