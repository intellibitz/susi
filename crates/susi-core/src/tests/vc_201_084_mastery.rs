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

/// Falsification: conflict resolution compares provenance as a *string*.
/// "p9" is lexically greater than "p10", so a stale write outranks a newer
/// one — the replica keeps the older record forever. Provenance ordering
/// is not lexical ordering.
#[test]
fn vc_201_084_mastery_lexical_provenance_keeps_stale_write() {
    let mut r = MemoryReplica::new();
    r.upsert(rec("m1", "old", "p9", false, false));
    // A peer's newer write (provenance p10) loses the string comparison.
    r.merge_from_peer(&[rec("m1", "new", "p10", false, false)]);
    assert_eq!(
        r.records["m1"].body, "old",
        "the stale write won — provenance is compared as a string"
    );
}

/// Falsification: deletion does not propagate on the export/merge path.
/// export_for_peers silently drops tombstoned records and merge_from_peer
/// never sees the tombstone set — a peer that only merges exports keeps
/// deleted content forever and can even re-broadcast it.
#[test]
fn vc_201_084_mastery_delete_does_not_propagate_via_export_merge() {
    let mut a = MemoryReplica::new();
    let mut b = MemoryReplica::new();
    a.upsert(rec("x", "shared", "p1", false, false));
    b.merge_from_peer(&a.export_for_peers());
    assert!(b.records.contains_key("x"));

    a.delete("x");
    // The documented replication path never carries the tombstone.
    b.merge_from_peer(&a.export_for_peers());
    assert!(
        b.records.contains_key("x"),
        "deleted record survives on the peer — exports carry no tombstones"
    );
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
