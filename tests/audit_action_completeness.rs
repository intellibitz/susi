#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Root-package architecture canary for T-DEEPSEEK-109.  It is deliberately
//! an integration test because the task's declared nextest command selects
//! the root package by default.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use susi_core::audit_export::{
    AuditAction, AuditActionKind, AuditChokePoint, AuditOutcome, AuditTarget, RecordingAuditSink,
};

#[test]
fn audit_action_completeness() {
    let names: BTreeSet<&str> = AuditActionKind::all()
        .iter()
        .map(|kind| kind.as_str())
        .collect();
    assert_eq!(names.len(), AuditActionKind::all().len());

    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let state = Arc::new(Mutex::new(0_u32));

    for (index, kind) in AuditActionKind::all().iter().enumerate() {
        let action = AuditAction::new(
            *kind,
            "architecture-test",
            AuditTarget::identifier(kind.as_str()),
            AuditOutcome::Succeeded,
            index as u64,
            index as u64 + 1,
            &format!("completeness/{index}"),
        )
        .unwrap();
        let state_for_mutation = Arc::clone(&state);
        choke
            .dispatch_mutation(&action, || {
                let mut value = state_for_mutation.lock().unwrap();
                *value += 1;
                Ok(())
            })
            .unwrap();
    }

    assert_eq!(
        *state.lock().unwrap(),
        AuditActionKind::all().len() as u32,
        "every state mutation must pass the audited mutation boundary"
    );
    let records = sink.records();
    assert_eq!(records.len(), AuditActionKind::all().len());
    assert!(records
        .iter()
        .zip(AuditActionKind::all())
        .all(|(record, expected)| record.action_kind == *expected && record.redacted));

    let mut bypass = AuditAction::from_legacy(
        "unregistered_mutation",
        "a bypassed state change",
        AuditOutcome::Succeeded,
    )
    .unwrap();
    bypass.redacted = false;
    assert!(choke.dispatch(&bypass).is_err());
    assert_eq!(sink.records().len(), AuditActionKind::all().len());
}

#[test]
fn dispatch_mutation_aborts_before_running_closure_when_action_is_unredacted() {
    let sink = RecordingAuditSink::default();
    let choke = AuditChokePoint::new(sink.clone());
    let mutated = Arc::new(Mutex::new(false));

    let mut invalid = AuditAction::new(
        AuditActionKind::ConfigurationChange,
        "mutation-guard",
        AuditTarget::identifier("config:key"),
        AuditOutcome::Succeeded,
        1,
        0,
        "completeness/unredacted",
    )
    .unwrap();
    invalid.redacted = false;

    let flag = Arc::clone(&mutated);
    let res = choke.dispatch_mutation(&invalid, || {
        *flag.lock().unwrap() = true;
        Ok(())
    });
    assert!(res.is_err());
    assert!(!*mutated.lock().unwrap());
    assert!(sink.records().is_empty());
}

#[test]
fn audit_action_from_legacy_hashes_detail_without_retaining_plaintext() {
    let secret_detail = "token=ghp_super_secret_value_12345";
    let action =
        AuditAction::from_legacy("shell_exec", secret_detail, AuditOutcome::Succeeded).unwrap();
    assert_eq!(action.action_kind, AuditActionKind::ToolExecution);
    assert_eq!(action.actor, "runtime");
    assert!(action.redacted);
    let payload = action.target.payload.as_ref().unwrap();
    assert_eq!(payload.bytes, secret_detail.len() as u64);
    assert_eq!(payload.hint, "action_detail");
    let json = serde_json::to_string(&action).unwrap();
    assert!(!json.contains("ghp_super_secret_value_12345"));
}

#[test]
fn audit_action_kinds_and_outcomes_expose_distinct_non_empty_labels() {
    for kind in AuditActionKind::all() {
        let label = kind.as_str();
        assert!(!label.is_empty());
        assert_eq!(label, label.to_ascii_lowercase());
    }
    let outcomes = [
        AuditOutcome::Succeeded,
        AuditOutcome::Denied,
        AuditOutcome::Failed,
        AuditOutcome::Cancelled,
        AuditOutcome::Partial,
        AuditOutcome::Uncertain,
    ];
    let labels: BTreeSet<&str> = outcomes.iter().map(|o| o.as_str()).collect();
    assert_eq!(labels.len(), outcomes.len());
}

#[test]
fn audit_action_new_rejects_empty_actor_or_correlation_id() {
    assert!(AuditAction::new(
        AuditActionKind::ModelCall,
        "",
        AuditTarget::identifier("model:test"),
        AuditOutcome::Succeeded,
        1,
        1,
        "corr-1",
    )
    .is_err());
    assert!(AuditAction::new(
        AuditActionKind::ModelCall,
        "actor-1",
        AuditTarget::identifier("model:test"),
        AuditOutcome::Succeeded,
        1,
        1,
        "",
    )
    .is_err());
}
