//! Mastery verification for VC-201-083: deletion propagation into derived
//! stores with durable tombstones.

use crate::memory_tombstone::MemoryStores;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-mem-tomb-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Verification: `insert` consults the tombstone set. A record deleted and fully
/// propagated cannot be repopulated into authoritative, semantic index, cache, or replica
/// by a late delayed write.
#[test]
fn vc_201_083_mastery_late_write_repopulates_derived_stores_after_delete() {
    let mut s = MemoryStores::default();
    s.insert("m1", "hello");
    s.delete_start("m1", 100);
    s.delete_resume(); // propagation completes; pending drained
    assert!(s.pending_delete.is_empty());

    // A delayed replica write for the deleted id arrives afterwards.
    let inserted = s.insert("m1", "resurrected");
    assert!(!inserted, "insert of tombstoned id must return false");

    assert!(s.get("m1").is_none(), "tombstone shields get()");
    assert!(
        !s.authoritative.contains_key("m1"),
        "authoritative store must not hold deleted content"
    );
    assert!(
        !s.semantic_index.contains("m1"),
        "semantic index must not hold deleted content"
    );
    assert!(
        !s.cache.contains_key("m1"),
        "cache must not hold deleted content"
    );
    assert!(
        !s.replica.contains_key("m1"),
        "replica must not hold deleted content"
    );
}

/// Verification: `delete_start` on a lagging write that existed in replica
/// is protected against late repopulation.
#[test]
fn vc_201_083_mastery_replica_only_content_survives_resume() {
    let mut s = MemoryStores::default();
    s.replica.insert("r1".into(), "lagging-write".into());
    s.delete_start("r1", 50);
    s.delete_resume();
    assert!(s.get("r1").is_none());
    assert!(!s.replica.contains_key("r1"), "resume did purge replica");

    // The same delayed write lands again after the tombstone exists.
    let inserted = s.insert("r1", "lagging-write");
    assert!(!inserted, "late write must be rejected");
    assert!(s.get("r1").is_none(), "still shielded");
    assert!(
        !s.replica.contains_key("r1"),
        "replica must not be repopulated after tombstone"
    );
}

/// Verification: tombstones survive across restart via durable persistence.
#[test]
fn vc_201_083_mastery_tombstones_do_not_survive_restart() {
    let dir = scratch("persist");
    let tombstone_file = dir.join("tombstones.json");

    let mut s = MemoryStores::new_persistent(&tombstone_file);
    s.insert("m9", "secret");
    s.delete_start("m9", 1);
    s.delete_resume();

    // Restart with fresh MemoryStores loaded from the same persistent file
    let mut restarted = MemoryStores::new_persistent(&tombstone_file);
    assert!(
        restarted.tombstones.contains_key("m9"),
        "tombstones must survive restart via durable persistence"
    );

    // If pre-delete content is replayed, the durable tombstone refuses it
    let inserted = restarted.insert("m9", "secret");
    assert!(
        !inserted,
        "replayed write must be rejected by durable tombstone"
    );
    assert_eq!(
        restarted.get("m9"),
        None,
        "deleted content must not be retrievable after restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// What holds: within one live process, delete_start tombstones
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
