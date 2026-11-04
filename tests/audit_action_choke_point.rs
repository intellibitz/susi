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

#[test]
fn audit_target_identifier_omits_payload_and_preserves_redaction_marker() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let action = AuditAction::new(
        AuditActionKind::ToolExecution,
        "agent-worker",
        AuditTarget::identifier("tool:read_file"),
        AuditOutcome::Succeeded,
        5,
        0,
        "trace/identifier-1",
    )
    .unwrap();

    assert!(action.target.payload.is_none());
    choke.dispatch(&action).unwrap();

    let records = sink.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].target.id, "tool:read_file");
    assert!(records[0].redacted);
}

#[test]
fn audit_action_rejects_empty_actor_target_or_correlation_id() {
    assert!(AuditAction::new(
        AuditActionKind::ModelCall,
        "",
        AuditTarget::identifier("provider:model"),
        AuditOutcome::Denied,
        1,
        0,
        "trace-1",
    )
    .is_err());
    assert!(AuditAction::new(
        AuditActionKind::ModelCall,
        "actor-1",
        AuditTarget::identifier(""),
        AuditOutcome::Denied,
        1,
        0,
        "trace-1",
    )
    .is_err());
    assert!(AuditAction::new(
        AuditActionKind::ModelCall,
        "actor-1",
        AuditTarget::identifier("provider:model"),
        AuditOutcome::Denied,
        1,
        0,
        "",
    )
    .is_err());
}

#[test]
fn audit_choke_point_records_denied_and_failed_outcomes_accurately() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    for (idx, outcome) in [AuditOutcome::Denied, AuditOutcome::Failed]
        .into_iter()
        .enumerate()
    {
        let action = AuditAction::new(
            AuditActionKind::EgressAttempt,
            "policy-guard",
            AuditTarget::identifier("rule:network_egress"),
            outcome,
            idx as u64 + 1,
            0,
            &format!("trace/outcome-{idx}"),
        )
        .unwrap();
        choke.dispatch(&action).unwrap();
    }
    let records = sink.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].outcome.as_str(), "denied");
    assert_eq!(records[1].outcome.as_str(), "failed");
}
