//! Mastery verification for VC-201-083: deletion propagation into derived
//! stores with durable tombstones.
//!
//! The cited tests show the happy path. The distinguishing properties are
//! that derived stores cannot be *repopulated* with deleted content after
//! the tombstone exists (a late replica write is exactly how real systems
//! resurrect), and that tombstones are durable beyond the process.

use crate::memory_tombstone::MemoryStores;

/// Falsification: `insert` never consults the tombstone set. A record
/// deleted and fully propagated (pending_delete drained by delete_resume)
/// is repopulated into authoritative, index, cache and replica by a single
/// late write — and nothing ever purges it again, because the id is no
/// longer pending. The tombstone shields `get()` but the deleted content
/// lives in every derived store, one tombstone check away from leaking.
#[test]
fn vc_201_083_mastery_late_write_repopulates_derived_stores_after_delete() {
    let mut s = MemoryStores::default();
    s.insert("m1", "hello");
    s.delete_start("m1", 100);
    s.delete_resume(); // propagation completes; pending drained
    assert!(s.pending_delete.is_empty());

    // A delayed replica write for the deleted id arrives afterwards.
    s.insert("m1", "resurrected");

    assert!(s.get("m1").is_none(), "tombstone still shields get()");
    assert_eq!(
        s.authoritative.get("m1").map(String::as_str),
        Some("resurrected"),
        "deleted content is back in the authoritative store"
    );
    assert!(s.semantic_index.contains("m1"));
    assert_eq!(s.cache.get("m1").map(String::as_str), Some("resurrected"));
    assert_eq!(s.replica.get("m1").map(String::as_str), Some("resurrected"));
    // And no mechanism will ever clean it: the id is not pending.
    assert!(s.pending_delete.is_empty());
    s.delete_resume(); // re-run propagation — nothing to do
    assert!(s.cache.contains_key("m1"), "deleted content is permanent");
}

/// Falsification: an id that was never deleted can be tombstoned out of
/// existence is harmless — but the reverse is not: `delete_start` on an
/// id that exists only in the *replica* (authoritative missed the write)
/// leaves the replica holding the content after resume, because insert
/// ordering is never validated against tombstone timestamps either.
#[test]
fn vc_201_083_mastery_replica_only_content_survives_resume() {
    let mut s = MemoryStores::default();
    s.replica.insert("r1".into(), "lagging-write".into());
    s.delete_start("r1", 50);
    s.delete_resume();
    assert!(s.get("r1").is_none());
    assert!(!s.replica.contains_key("r1"), "resume did purge replica");

    // The same delayed write lands again after the tombstone exists.
    s.insert("r1", "lagging-write");
    assert!(s.get("r1").is_none(), "still shielded");
    assert_eq!(
        s.replica.get("r1").map(String::as_str),
        Some("lagging-write"),
        "replica repopulated despite the tombstone"
    );
}

/// Falsification: tombstones are in-memory only. Nothing in MemoryStores
/// persists them — a process restart (a fresh instance) has no record of
/// any deletion, and any snapshot of derived stores resurrects the content
/// wholesale. "Durable" requires persistence that does not exist here.
#[test]
fn vc_201_083_mastery_tombstones_do_not_survive_restart() {
    let mut s = MemoryStores::default();
    s.insert("m9", "secret");
    s.delete_start("m9", 1);
    s.delete_resume();

    let restarted = MemoryStores::default();
    assert!(
        restarted.tombstones.is_empty(),
        "a restart forgets every deletion — tombstones are volatile"
    );
    // If a pre-delete snapshot of the cache were replayed into the new
    // process there is no tombstone to stop it.
    let mut restarted = restarted;
    restarted.cache.insert("m9".into(), "secret".into());
    assert_eq!(
        restarted.get("m9"),
        Some("secret"),
        "deleted content is retrievable after restart — no durable tombstone"
    );
}

/// What does hold: within one live process, delete_start tombstones
/// immediately, get() refuses the id even when the cache is poisoned,
/// and delete_resume drains pending propagation.
#[test]
fn vc_201_083_mastery_in_process_guarantees_hold() {
    let mut s = MemoryStores::default();
    s.insert("m3", "y");
    s.delete_start("m3", 1);
    assert!(s.get("m3").is_none());
    s.cache.insert("m3".into(), "resurrect".into());
    assert!(s.get("m3").is_none(), "tombstone blocks cache read-through");
    assert!(s.pending_delete.contains("m3"));
    s.delete_resume();
    assert!(s.pending_delete.is_empty());
    assert!(!s.cache.contains_key("m3"));
    assert!(!s.semantic_index.contains("m3"));
    assert!(!s.replica.contains_key("m3"));
}
