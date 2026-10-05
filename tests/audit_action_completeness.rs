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
