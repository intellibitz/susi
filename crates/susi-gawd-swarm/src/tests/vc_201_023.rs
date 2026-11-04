use crate::side_effects::{after_crash_retry, may_retry_after_crash, ActionClass, ActionOutcome};

#[test]
fn vc_201_023_retry_only_supported_classes() {
    assert!(may_retry_after_crash(ActionClass::ReadOnly));
    assert!(may_retry_after_crash(ActionClass::Idempotent));
    assert!(may_retry_after_crash(ActionClass::Reconcilable));
    assert!(!may_retry_after_crash(ActionClass::NonRetryable));
}

#[test]
fn vc_201_023_non_retryable_surfaces_uncertain_external() {
    let prior = ActionOutcome {
        class: ActionClass::NonRetryable,
        uncertain_external: false,
        completed: false,
    };
    let out = after_crash_retry(&prior);
    assert!(out.uncertain_external);
    assert!(!out.completed);
}

#[test]
fn vc_201_023_idempotent_retries_cleanly() {
    let prior = ActionOutcome {
        class: ActionClass::Idempotent,
        uncertain_external: false,
        completed: false,
    };
    let out = after_crash_retry(&prior);
    assert!(out.completed);
    assert!(!out.uncertain_external);
}

#[test]
fn vc_201_023_read_only_vs_reconcilable_crash_retry_outcomes() {
    let read_only_prior = ActionOutcome {
        class: ActionClass::ReadOnly,
        uncertain_external: false,
        completed: false,
    };
    let read_only_out = after_crash_retry(&read_only_prior);
    assert!(read_only_out.completed);
    assert!(!read_only_out.uncertain_external);

    let reconcilable_prior = ActionOutcome {
        class: ActionClass::Reconcilable,
        uncertain_external: false,
        completed: false,
    };
    let reconcilable_out = after_crash_retry(&reconcilable_prior);
    assert!(reconcilable_out.completed);
    assert!(reconcilable_out.uncertain_external);
}

#[test]
fn vc_201_023_action_class_equality_and_clone_hold_for_all_variants() {
    let classes = [
        ActionClass::ReadOnly,
        ActionClass::Idempotent,
        ActionClass::Reconcilable,
        ActionClass::NonRetryable,
    ];
    for c in classes {
        assert_eq!(c.clone(), c);
    }
    assert_ne!(ActionClass::ReadOnly, ActionClass::NonRetryable);
}
