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

/// Classify a swarm tool invocation for crash-retry policy (dispatch wiring).
#[must_use]
pub fn classify_tool_action(tool: &str) -> ActionClass {
    match tool {
        "read_file" | "list_dir" | "search" | "grep" => ActionClass::ReadOnly,
        "exec_command" | "write_file" | "apply_patch" => ActionClass::Idempotent,
        "http_call" | "peer_dispatch" => ActionClass::Reconcilable,
        _ => ActionClass::NonRetryable,
    }
}

/// Decide whether a crashed dispatch may retry a tool, and what outcome to
/// record for reconciliation.
#[must_use]
pub fn reconcile_dispatch_side_effect(tool: &str, prior: Option<&ActionOutcome>) -> ActionOutcome {
    match prior {
        Some(p) => after_crash_retry(p),
        None => ActionOutcome {
            class: classify_tool_action(tool),
            uncertain_external: false,
            completed: false,
        },
    }
}
