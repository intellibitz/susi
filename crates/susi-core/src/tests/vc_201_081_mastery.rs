//! Mastery verification for VC-201-081: durable memory carrying
//! workspace, receipt, revision, classification, expiry, and owner, with
//! reads rejecting expired or unauthorized records.

use crate::memory_provenance::{
    DataClassification, MemoryAccessError, MemoryRecord, MemoryStore, Provenance, ProvenanceSource,
};
use crate::receipt_archive::ArchivedReceipt;
use std::path::Path;

fn rec(id: &str, source: ProvenanceSource, recorded: u64, retention: u64) -> MemoryRecord {
    MemoryRecord {
        id: id.into(),
        body: "note".into(),
        provenance: Provenance {
            source,
            workspace: "/home/susi/workspace".into(),
            receipt: Some("rcpt-001".into()),
            revision: "git-commit-hash-abc".into(),
            classification: DataClassification::Internal,
            owner: "operator-alice".into(),
            recorded_unix: recorded,
            retention_secs: retention,
        },
    }
}

/// Verification: 'distinguish evidence from model-generated assertions'.
/// Evidence carries structured receipt and verifier proofs (`is_evidence() == true`),
/// while model assertions carry model and prompt hash (`is_model_assertion() == true`).
/// Model assertions cannot pose as evidence.
#[test]
fn vc_201_081_mastery_self_declared_provenance() {
    let evidence_src = ProvenanceSource::Evidence {
        receipt_id: "tool-receipt-99".into(),
        verifier: "sha256-hash-check".into(),
    };
    let model_src = ProvenanceSource::ModelAssertion {
        model: "claude-3-opus".into(),
        prompt_hash: "hash-42".into(),
    };

    assert!(evidence_src.is_evidence());
    assert!(!evidence_src.is_model_assertion());
    assert_eq!(evidence_src.authoritative_name(), "evidence");

    assert!(!model_src.is_evidence());
    assert!(model_src.is_model_assertion());
    assert_eq!(model_src.authoritative_name(), "model-assertion");

    let r_evidence = rec("m1", evidence_src, 100, 50);
    let r_model = rec("m2", model_src, 100, 50);

    assert_eq!(r_evidence.authoritative_source(), "evidence");
    assert_eq!(r_model.authoritative_source(), "model-assertion");
}

/// Verification: the 5 claimed fields exist and are attached to Provenance:
/// workspace, receipt, revision, classification, and owner.
#[test]
fn vc_201_081_mastery_claimed_fields_absent() {
    let p = Provenance {
        source: ProvenanceSource::from("user"),
        workspace: "/var/lib/susi/workspace".into(),
        receipt: Some("receipt-xyz-42".into()),
        revision: "rev-2026-10".into(),
        classification: DataClassification::Confidential,
        owner: "agent-devin".into(),
        recorded_unix: 1000,
        retention_secs: 3600,
    };

    assert_eq!(p.workspace, "/var/lib/susi/workspace");
    assert_eq!(p.receipt.as_deref(), Some("receipt-xyz-42"));
    assert_eq!(p.revision, "rev-2026-10");
    assert_eq!(p.classification, DataClassification::Confidential);
    assert_eq!(p.owner, "agent-devin");
    assert_eq!(p.retention_secs, 3600);
}

/// Verification: 'reads reject expired or unauthorized records'.
/// The read path enforces expiry deadlines, caller identity matching the owner,
/// and security clearance matching the classification level.
#[test]
fn vc_201_081_mastery_expired_records_are_readable() {
    let mut store = MemoryStore::default();
    let r = rec(
        "m1",
        ProvenanceSource::User {
            user_id: "alice".into(),
        },
        100,
        50,
    );
    store.insert(r);

    // 1. Authorized read before expiration succeeds
    let read_ok = store.read("m1", "operator-alice", DataClassification::Internal, 140);
    assert_eq!(read_ok, Ok("note"));

    // 2. Read after expiration fails with Expired error
    let read_expired = store.read("m1", "operator-alice", DataClassification::Internal, 200);
    assert!(matches!(
        read_expired,
        Err(MemoryAccessError::Expired {
            expired_at: 150,
            now: 200
        })
    ));

    // 3. Unauthorized caller fails with Unauthorized error
    let read_unauth = store.read("m1", "intruder-bob", DataClassification::Internal, 140);
    assert!(matches!(
        read_unauth,
        Err(MemoryAccessError::Unauthorized { .. })
    ));

    // 4. Insufficient clearance fails with ClassificationDenied error
    let read_denied = store.read("m1", "operator-alice", DataClassification::Public, 140);
    assert!(matches!(
        read_denied,
        Err(MemoryAccessError::ClassificationDenied {
            required: DataClassification::Internal,
            granted: DataClassification::Public,
        })
    ));
}

/// Verification: retention_secs == 0 denotes permanent/indefinite retention,
/// and does NOT make the record born-expired at its own recorded instant.
#[test]
fn vc_201_081_mastery_zero_retention_is_born_expired() {
    let r = rec(
        "m1",
        ProvenanceSource::User {
            user_id: "root".into(),
        },
        100,
        0,
    );
    assert!(
        !r.expired(100),
        "recorded at 100 with permanent retention must not be expired at 100"
    );
    assert!(
        !r.expired(1_000_000),
        "permanent retention records remain unexpired into the future"
    );
}

/// Verification: nominal expiry arithmetic holds.
#[test]
fn vc_201_081_mastery_expiry_math_holds() {
    let r = rec(
        "m1",
        ProvenanceSource::User {
            user_id: "user".into(),
        },
        100,
        50,
    );
    assert!(!r.expired(149));
    assert!(r.expired(150));
    // And a future-dated record is not yet expired.
    let f = rec(
        "m2",
        ProvenanceSource::User {
            user_id: "user".into(),
        },
        1_000_000,
        50,
    );
    assert!(!f.expired(200));
}

/// Verification: Production caller wiring from ArchivedReceipt to MemoryRecord and MemoryStore.
#[test]
fn vc_201_081_mastery_production_caller_wired() {
    let receipt = ArchivedReceipt {
        schema: "susi/receipt_archive/v1".into(),
        kind: "tool_execution".into(),
        session_id: "session-42".into(),
        mission_goal_hash: "goal-hash-99".into(),
        training_intent: None,
        receipt_id: "rcpt-real-1".into(),
        tool: "cargo_build".into(),
        arguments: "{\"target\": \"release\"}".into(),
        observed_at: 500,
        output_hash: "hash-out-123".into(),
        successful: true,
        archived_at: 501,
        training_staged: Some(true),
    };

    let ws = Path::new("/srv/workspaces/proj-1");
    let mem_record = receipt.to_memory_record(ws, "agent-devin");

    assert_eq!(mem_record.id, "rcpt-real-1");
    assert_eq!(mem_record.provenance.workspace, "/srv/workspaces/proj-1");
    assert_eq!(mem_record.provenance.owner, "agent-devin");
    assert_eq!(mem_record.provenance.revision, "session-42");
    assert!(mem_record.provenance.source.is_evidence());

    let mut store = MemoryStore::default();
    store.insert(mem_record);

    let read_result = store.read(
        "rcpt-real-1",
        "agent-devin",
        DataClassification::Internal,
        1000,
    );
    assert!(read_result.is_ok());
    assert!(read_result.unwrap().contains("cargo_build"));
}
