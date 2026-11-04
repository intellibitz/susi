use crate::memory_provenance::{
    DataClassification, MemoryAccessError, MemoryRecord, MemoryStore, Provenance, ProvenanceSource,
};

#[test]
fn vc_201_081_records_provenance_and_retention() {
    let r = MemoryRecord {
        id: "m1".into(),
        body: "note".into(),
        provenance: Provenance {
            source: ProvenanceSource::from("user"),
            workspace: "/workspace".into(),
            receipt: Some("rcpt-1".into()),
            revision: "rev-1".into(),
            classification: DataClassification::Internal,
            owner: "user".into(),
            recorded_unix: 100,
            retention_secs: 50,
        },
    };
    assert_eq!(r.authoritative_source(), "user");
    assert!(!r.expired(140));
    assert!(r.expired(150));
}

#[test]
fn vc_201_081_zero_retention_never_expires_and_provenance_sources_classify_accurately() {
    let r = MemoryRecord::new(
        "perm-1",
        "permanent",
        Provenance {
            source: ProvenanceSource::from("evidence"),
            workspace: "/ws".into(),
            receipt: Some("r1".into()),
            revision: "rev".into(),
            classification: DataClassification::Public,
            owner: "alice".into(),
            recorded_unix: 100,
            retention_secs: 0,
        },
    );
    assert!(!r.expired(u64::MAX));
    assert!(ProvenanceSource::from("evidence").is_evidence());
    assert!(ProvenanceSource::from("model").is_model_assertion());
    assert_eq!(
        ProvenanceSource::from("system").authoritative_name(),
        "system"
    );
    assert_eq!(
        ProvenanceSource::from("custom-origin").authoritative_name(),
        "custom-origin"
    );
}

#[test]
fn vc_201_081_read_body_enforces_expiration_owner_and_classification_clearance() {
    let r = MemoryRecord::new(
        "sec-1",
        "secret-body",
        Provenance {
            source: ProvenanceSource::from("user"),
            workspace: "/ws".into(),
            receipt: None,
            revision: "rev".into(),
            classification: DataClassification::Confidential,
            owner: "alice".into(),
            recorded_unix: 100,
            retention_secs: 50,
        },
    );
    assert!(matches!(
        r.read_body("alice", DataClassification::Confidential, 160),
        Err(MemoryAccessError::Expired { .. })
    ));
    assert!(matches!(
        r.read_body("bob", DataClassification::Confidential, 120),
        Err(MemoryAccessError::Unauthorized { .. })
    ));
    assert!(matches!(
        r.read_body("alice", DataClassification::Internal, 120),
        Err(MemoryAccessError::ClassificationDenied { .. })
    ));
    assert_eq!(
        r.read_body("alice", DataClassification::Confidential, 120),
        Ok("secret-body")
    );
    assert_eq!(
        r.read_body("admin", DataClassification::Restricted, 120),
        Ok("secret-body")
    );
}

#[test]
fn vc_201_081_memory_store_insert_get_and_read_missing_or_authorized() {
    let mut store = MemoryStore::default();
    assert!(matches!(
        store.read("missing", "alice", DataClassification::Public, 10),
        Err(MemoryAccessError::Unauthorized { .. })
    ));

    store.insert(MemoryRecord::new(
        "doc-1",
        "public-body",
        Provenance {
            source: ProvenanceSource::from("system"),
            workspace: "/ws".into(),
            receipt: None,
            revision: "rev".into(),
            classification: DataClassification::Public,
            owner: "alice".into(),
            recorded_unix: 1,
            retention_secs: 0,
        },
    ));
    assert!(store.get_record("doc-1").is_some());
    assert_eq!(
        store.read("doc-1", "alice", DataClassification::Public, 10),
        Ok("public-body")
    );
}
