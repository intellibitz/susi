use crate::memory_provenance::{MemoryRecord, Provenance};

#[test]
fn vc_201_081_records_provenance_and_retention() {
    let r = MemoryRecord {
        id: "m1".into(),
        body: "note".into(),
        provenance: Provenance {
            source: "user".into(),
            recorded_unix: 100,
            retention_secs: 50,
        },
    };
    assert_eq!(r.authoritative_source(), "user");
    assert!(!r.expired(140));
    assert!(r.expired(150));
}
