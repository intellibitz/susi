#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Root-package acceptance canary for T-DEEPSEEK-108.  The task's declared
//! workspace command selects the root package by default, so this test keeps
//! that command non-vacuous while exercising the production core contract.

use susi_core::audit_export::{
    AuditAction, AuditActionKind, AuditChokePoint, AuditOutcome, AuditTarget, RecordingAuditSink,
};

#[test]
fn audit_action_choke_point() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let payload = "private prompt with sk-proj-not-for-audit";
    let action = AuditAction::new(
        AuditActionKind::ModelCall,
        "root-acceptance",
        AuditTarget::payload_ref("provider:model", payload.as_bytes(), "prompt"),
        AuditOutcome::Succeeded,
        12,
        34,
        "acceptance/audit-action-1",
    )
    .unwrap();

    choke.dispatch(&action).unwrap();

    let records = sink.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action_kind, AuditActionKind::ModelCall);
    assert_eq!(records[0].duration_ms, 12);
    assert_eq!(records[0].cost_micros, 34);
    assert!(records[0].redacted);
    let serialized = serde_json::to_string(&records[0]).unwrap();
    assert!(!serialized.contains("private prompt"));
    assert!(!serialized.contains("sk-proj-not-for-audit"));
}
