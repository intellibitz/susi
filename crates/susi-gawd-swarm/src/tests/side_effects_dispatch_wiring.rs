//! Wiring: side-effect classify/reconcile in MissionDag dispatch (T-INTELLIBITZ-7).

use crate::dag::MissionDag;
use crate::side_effect_journal::DispatchDecision;
use crate::side_effects::{classify_tool_action, ActionClass, ActionOutcome};

#[test]
fn side_effects_dispatch_wiring() {
    let mut dag = MissionDag::new("side effect wiring");

    assert_eq!(classify_tool_action("read_file"), ActionClass::ReadOnly);
    // Arbitrary shell/patch calls are Reconcilable, never Idempotent by name
    // (T-DEVIN-10) — replay requires reconciling the uncertain intent.
    assert_eq!(
        classify_tool_action("exec_command"),
        ActionClass::Reconcilable
    );
    assert_eq!(
        classify_tool_action("apply_patch"),
        ActionClass::Reconcilable
    );
    assert_eq!(classify_tool_action("http_call"), ActionClass::Reconcilable);
    assert_eq!(
        classify_tool_action("one_shot_pay"),
        ActionClass::NonRetryable
    );

    let first = dag.record_side_effect(0, "exec_command", None);
    assert_eq!(first.class, ActionClass::Reconcilable);
    assert!(!first.completed);
    assert!(dag.may_retry_node(0));

    let after_crash = dag.record_side_effect(0, "exec_command", Some(&first));
    assert!(after_crash.completed);
    assert!(after_crash.uncertain_external);

    let non_retry = ActionOutcome {
        class: ActionClass::NonRetryable,
        uncertain_external: false,
        completed: false,
    };
    let blocked = dag.record_side_effect(0, "one_shot_pay", Some(&non_retry));
    assert!(!blocked.completed);
    assert!(blocked.uncertain_external);
    assert!(!dag.may_retry_node(0));
}

#[test]
fn side_effects_dispatch_wiring_read_only_tools_retry_cleanly_without_uncertainty() {
    assert_eq!(classify_tool_action("list_dir"), ActionClass::ReadOnly);
    assert_eq!(classify_tool_action("search"), ActionClass::ReadOnly);
    assert_eq!(classify_tool_action("grep"), ActionClass::ReadOnly);

    let mut dag = MissionDag::new("read-only retry");
    let initial = dag.record_side_effect(0, "grep", None);
    assert_eq!(initial.class, ActionClass::ReadOnly);
    assert!(!initial.uncertain_external);
    assert!(!initial.completed);
    assert!(dag.may_retry_node(0));

    let retried = dag.record_side_effect(0, "grep", Some(&initial));
    assert_eq!(retried.class, ActionClass::ReadOnly);
    assert!(!retried.uncertain_external);
    assert!(retried.completed);
    assert!(dag.may_retry_node(0));
}

#[test]
fn side_effects_dispatch_wiring_unreconciled_intent_journal_blocks_may_retry_node() {
    let dag = MissionDag::new("journal crash block");
    let intent_id = dag.record_dispatch_intent(0, "write_file", "{\"path\":\"out.txt\"}");

    // Simulate crash before mark_intent_executed: persisted recovery marks it Uncertain.
    let persisted = dag.to_persisted("mission-journal");
    let recovered = MissionDag::from_persisted(&persisted);
    assert!(
        !recovered.may_retry_node(0),
        "unreconciled intent after crash must block node retry"
    );

    // Marking executed on the live DAG keeps retry permitted.
    dag.mark_intent_executed(&intent_id);
    assert!(dag.may_retry_node(0));
}

#[test]
fn side_effects_dispatch_wiring_prepare_dispatch_intent_deduplicates_acknowledged_receipt() {
    assert_eq!(
        classify_tool_action("write_file"),
        ActionClass::Reconcilable
    );
    assert_eq!(
        classify_tool_action("peer_dispatch"),
        ActionClass::Reconcilable
    );

    let dag = MissionDag::new("idempotent receipt");
    let first = dag.prepare_dispatch_intent(0, "write_file", "{\"path\":\"a\"}", Some("k-1"));
    let intent_id = match first {
        DispatchDecision::Dispatch { intent_id } => intent_id,
        other => panic!("expected Dispatch, got {other:?}"),
    };
    dag.mark_intent_executed_with_receipt(&intent_id, "wrote 12 bytes");

    let replay = dag.prepare_dispatch_intent(0, "write_file", "{\"path\":\"a\"}", Some("k-1"));
    match replay {
        DispatchDecision::AlreadyExecuted {
            intent_id: replayed_id,
            receipt,
        } => {
            assert_eq!(replayed_id, intent_id);
            assert!(!receipt.output_digest.is_empty());
        }
        other => panic!("expected AlreadyExecuted, got {other:?}"),
    }
}

#[test]
fn side_effects_dispatch_wiring_unknown_tool_defaults_fail_closed_to_non_retryable() {
    assert_eq!(
        classify_tool_action("custom_external_webhook"),
        ActionClass::NonRetryable
    );
    let mut dag = MissionDag::new("unknown tool default");
    let first = dag.record_side_effect(0, "custom_external_webhook", None);
    assert_eq!(first.class, ActionClass::NonRetryable);
    assert!(!first.uncertain_external);
    assert!(!dag.may_retry_node(0));

    let second = dag.record_side_effect(0, "custom_external_webhook", Some(&first));
    assert!(second.uncertain_external);
    assert!(!dag.may_retry_node(0));
}
