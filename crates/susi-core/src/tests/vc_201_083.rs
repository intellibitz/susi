use crate::memory_tombstone::MemoryStores;

#[test]
fn vc_201_083_delete_propagates_with_tombstone() {
    let mut s = MemoryStores::default();
    s.insert("m1", "hello");
    s.delete_start("m1", 100);
    assert!(s.get("m1").is_none());
    assert!(s.tombstones.contains_key("m1"));
    // Derived still has stale until resume
    assert!(s.cache.contains_key("m1"));
    s.delete_resume();
    assert!(!s.cache.contains_key("m1"));
    assert!(!s.semantic_index.contains("m1"));
    assert!(!s.replica.contains_key("m1"));
    assert!(s.get("m1").is_none());
}

#[test]
fn vc_201_083_interrupted_delete_resumes() {
    let mut s = MemoryStores::default();
    s.insert("m2", "x");
    s.delete_start("m2", 1);
    assert!(s.pending_delete.contains("m2"));
    s.delete_resume();
    assert!(s.pending_delete.is_empty());
}

#[test]
fn vc_201_083_retrieval_cannot_resurrect() {
    let mut s = MemoryStores::default();
    s.insert("m3", "y");
    s.delete_start("m3", 1);
    s.cache.insert("m3".into(), "resurrect".into());
    assert!(s.get("m3").is_none(), "tombstone blocks cache resurrection");
}
