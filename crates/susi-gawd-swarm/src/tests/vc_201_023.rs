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
