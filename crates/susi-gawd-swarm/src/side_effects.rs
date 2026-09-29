//! Classify and reconcile task side effects (VC-201-023).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    ReadOnly,
    Idempotent,
    Reconcilable,
    NonRetryable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub class: ActionClass,
    pub uncertain_external: bool,
    pub completed: bool,
}

#[must_use]
pub fn may_retry_after_crash(class: ActionClass) -> bool {
    matches!(
        class,
        ActionClass::ReadOnly | ActionClass::Idempotent | ActionClass::Reconcilable
    )
}

#[must_use]
pub fn after_crash_retry(prior: &ActionOutcome) -> ActionOutcome {
    if !may_retry_after_crash(prior.class) {
        return ActionOutcome {
            class: prior.class,
            uncertain_external: true,
            completed: false,
        };
    }
    ActionOutcome {
        class: prior.class,
        uncertain_external: matches!(prior.class, ActionClass::Reconcilable) && !prior.completed,
        completed: true,
    }
}
