//! Test for audit action choke point: every state-changing action emits one audit record through one interface.
//!
//! Vector VC-202-017: Governance & compliance
//! Mastery target: Prove that all state-changing actions (model load, configuration change, deployment, etc.)
//! emit exactly one audit record through a unified choke point, preventing audit gaps and double-recording.

use crate::audit_export::{
    AuditAction, AuditActionKind, AuditChokePoint, AuditOutcome, AuditTarget, RecordingAuditSink,
};

#[test]
fn audit_action_choke_point_production_contract() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let prompt = "deploy with bearer sk-proj-secret and private prompt";
    let action = AuditAction::new(
        AuditActionKind::ToolExecution,
        "agent-7",
        AuditTarget::payload_ref("deployment", prompt.as_bytes(), "tool_input"),
        AuditOutcome::Succeeded,
        42,
        17,
        "mission-7/tool-1",
    )
    .unwrap();

    choke.dispatch(&action).unwrap();

    let records = sink.records();
    assert_eq!(records.len(), 1, "one action must produce one record");
    let record = &records[0];
    assert_eq!(record.action_kind, AuditActionKind::ToolExecution);
    assert_eq!(record.outcome, AuditOutcome::Succeeded);
    assert_eq!(record.duration_ms, 42);
    assert_eq!(record.cost_micros, 17);
    assert_eq!(record.correlation_id, "mission-7/tool-1");
    assert!(record.redacted);
    assert_eq!(
        record.target.payload.as_ref().unwrap().bytes,
        prompt.len() as u64
    );
    let serialized = serde_json::to_string(record).unwrap();
    assert!(!serialized.contains("bearer"));
    assert!(!serialized.contains("private prompt"));
    assert!(!serialized.contains("sk-proj-secret"));
}

#[test]
fn audit_action_choke_point_rejects_unredacted_mutation() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let mut action = AuditAction::from_legacy(
        "write_file",
        "contents never enter an audit record",
        AuditOutcome::Succeeded,
    )
    .unwrap();
    action.redacted = false;

    assert!(choke.dispatch(&action).is_err());
    assert!(sink.records().is_empty());
}
