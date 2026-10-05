//! Test for audit action choke point: every state-changing action emits one audit record through one interface.
//!
//! Vector VC-202-017: Governance & compliance
//! Mastery target: Prove that all state-changing actions (model load, configuration change, deployment, etc.)
//! emit exactly one audit record through a unified choke point, preventing audit gaps and double-recording.
//!
//! This test exercises:
//! - AuditEvent struct with required fields (ts_unix, action, actor, outcome)
//! - Unified export_ndjson() as the single choke point for audit serialization
//! - Round-trip parse_ndjson_line() to verify record integrity
//! - Outcome field tracks action success/failure (ok, denied, failed)

use crate::audit_export::{
    export_ndjson, parse_ndjson_line, AuditAction, AuditActionKind, AuditChokePoint, AuditEvent,
    AuditOutcome, AuditTarget, RecordingAuditSink,
};

#[test]
fn audit_action_choke_point_unified_interface() {
    // Every state-changing action creates an AuditEvent with consistent schema:
    // timestamp, action name, actor identity, outcome.
    let events = vec![
        AuditEvent {
            ts_unix: 1000,
            action: "model_load".into(),
            actor: "daemon".into(),
            outcome: "ok".into(),
        },
        AuditEvent {
            ts_unix: 1001,
            action: "config_change".into(),
            actor: "admin".into(),
            outcome: "ok".into(),
        },
        AuditEvent {
            ts_unix: 1002,
            action: "deploy_request".into(),
            actor: "user".into(),
            outcome: "denied".into(),
        },
    ];

    // All records flow through unified export_ndjson() choke point.
    let ndjson = export_ndjson(&events);
    let lines: Vec<&str> = ndjson.lines().collect();

    // Exactly 3 records exported (no loss, no duplication).
    assert_eq!(lines.len(), 3, "Expected exactly 3 audit records");

    // Each line is a valid JSON record.
    for (i, line) in lines.iter().enumerate() {
        let parsed =
            parse_ndjson_line(line).unwrap_or_else(|_| panic!("Line {} failed to parse", i));
        assert!(
            !parsed["action"].is_null(),
            "Line {} missing action field",
            i
        );
        assert!(!parsed["actor"].is_null(), "Line {} missing actor field", i);
        assert!(
            !parsed["outcome"].is_null(),
            "Line {} missing outcome field",
            i
        );
    }

    // Verify record integrity: ts, action, actor, outcome preserved round-trip.
    let parsed_0 = parse_ndjson_line(lines[0]).unwrap();
    assert_eq!(parsed_0["action"], "model_load");
    assert_eq!(parsed_0["actor"], "daemon");
    assert_eq!(parsed_0["outcome"], "ok");

    let parsed_2 = parse_ndjson_line(lines[2]).unwrap();
    assert_eq!(parsed_2["action"], "deploy_request");
    assert_eq!(parsed_2["outcome"], "denied");
}

#[test]
fn audit_action_choke_point_outcome_tracking() {
    // Outcomes track action success and denial reasons.
    let success = AuditEvent {
        ts_unix: 2000,
        action: "auth_login".into(),
        actor: "user_42".into(),
        outcome: "ok".into(),
    };

    let denied = AuditEvent {
        ts_unix: 2001,
        action: "auth_login".into(),
        actor: "user_43".into(),
        outcome: "denied".into(),
    };

    let failed = AuditEvent {
        ts_unix: 2002,
        action: "model_warmup".into(),
        actor: "daemon".into(),
        outcome: "failed".into(),
    };

    let events = vec![success, denied, failed];
    let ndjson = export_ndjson(&events);
    let lines: Vec<&str> = ndjson.lines().collect();

    assert_eq!(parse_ndjson_line(lines[0]).unwrap()["outcome"], "ok");
    assert_eq!(parse_ndjson_line(lines[1]).unwrap()["outcome"], "denied");
    assert_eq!(parse_ndjson_line(lines[2]).unwrap()["outcome"], "failed");
}

#[test]
fn audit_action_choke_point_actor_identity() {
    // Actor field captures who initiated the action.
    // Enables attribution for compliance investigations.
    let events = vec![
        AuditEvent {
            ts_unix: 3000,
            action: "resource_delete".into(),
            actor: "admin".into(),
            outcome: "ok".into(),
        },
        AuditEvent {
            ts_unix: 3001,
            action: "api_request".into(),
            actor: "service_account_oauth".into(),
            outcome: "ok".into(),
        },
    ];

    let ndjson = export_ndjson(&events);
    let lines: Vec<&str> = ndjson.lines().collect();

    assert_eq!(parse_ndjson_line(lines[0]).unwrap()["actor"], "admin");
    assert_eq!(
        parse_ndjson_line(lines[1]).unwrap()["actor"],
        "service_account_oauth"
    );
}

#[test]
fn audit_action_choke_point_empty_queue() {
    // Empty audit queue produces empty string (no orphaned records).
    let ndjson = export_ndjson(&[]);
    assert_eq!(ndjson, "");
}

#[test]
fn audit_action_choke_point_single_record() {
    // Single record exports and round-trips correctly.
    let event = AuditEvent {
        ts_unix: 4000,
        action: "test_action".into(),
        actor: "test_actor".into(),
        outcome: "ok".into(),
    };

    let ndjson = export_ndjson(&[event]);
    let parsed = parse_ndjson_line(&ndjson).expect("Failed to parse single record");

    assert_eq!(parsed["ts"], 4000);
    assert_eq!(parsed["action"], "test_action");
    assert_eq!(parsed["actor"], "test_actor");
    assert_eq!(parsed["outcome"], "ok");
}

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
