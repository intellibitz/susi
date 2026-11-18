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
///
/// Honesty fix (T-DEVIN-10): `exec_command`, `apply_patch`, and `write_file`
/// are NOT idempotent — an arbitrary shell command or a crash-torn write
/// cannot be replayed blindly. They are `Reconcilable`: replay is allowed
/// only after the uncertain intent is reconciled against external state.
/// `Idempotent` remains for calls explicitly proven replay-safe.
#[must_use]
pub fn classify_tool_action(tool: &str) -> ActionClass {
    match tool {
        "read_file" | "list_dir" | "search" | "grep" => ActionClass::ReadOnly,
        "exec_command" | "write_file" | "apply_patch" | "http_call" | "peer_dispatch" => {
            ActionClass::Reconcilable
        }
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

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn may_retry_after_crash_allows_read_only_idempotent_and_reconcilable_only() {
        assert!(may_retry_after_crash(ActionClass::ReadOnly));
        assert!(may_retry_after_crash(ActionClass::Idempotent));
        assert!(may_retry_after_crash(ActionClass::Reconcilable));
        assert!(!may_retry_after_crash(ActionClass::NonRetryable));
    }

    #[test]
    fn classify_tool_action_distinguishes_read_only_reconcilable_and_non_retryable() {
        assert_eq!(classify_tool_action("read_file"), ActionClass::ReadOnly);
        assert_eq!(classify_tool_action("grep"), ActionClass::ReadOnly);
        assert_eq!(
            classify_tool_action("exec_command"),
            ActionClass::Reconcilable
        );
        assert_eq!(
            classify_tool_action("write_file"),
            ActionClass::Reconcilable
        );
        assert_eq!(
            classify_tool_action("custom_external_drop"),
            ActionClass::NonRetryable
        );
    }

    #[test]
    fn after_crash_retry_marks_incomplete_reconcilable_uncertain_and_blocks_non_retryable() {
        let incomplete_reconcilable = ActionOutcome {
            class: ActionClass::Reconcilable,
            uncertain_external: false,
            completed: false,
        };
        let retried = after_crash_retry(&incomplete_reconcilable);
        assert!(retried.uncertain_external);
        assert!(retried.completed);

        let non_retryable = ActionOutcome {
            class: ActionClass::NonRetryable,
            uncertain_external: false,
            completed: false,
        };
        let blocked = after_crash_retry(&non_retryable);
        assert!(blocked.uncertain_external);
        assert!(!blocked.completed);
    }

    #[test]
    fn reconcile_dispatch_side_effect_initializes_fresh_outcome_or_advances_prior() {
        let fresh = reconcile_dispatch_side_effect("apply_patch", None);
        assert_eq!(fresh.class, ActionClass::Reconcilable);
        assert!(!fresh.uncertain_external);
        assert!(!fresh.completed);

        let resumed = reconcile_dispatch_side_effect("apply_patch", Some(&fresh));
        assert_eq!(resumed.class, ActionClass::Reconcilable);
        assert!(resumed.uncertain_external);
        assert!(resumed.completed);
    }

    #[test]
    fn action_outcome_serde_roundtrip_and_completed_reconcilable_clears_uncertain_flag() {
        let completed_reconcilable = ActionOutcome {
            class: ActionClass::Reconcilable,
            uncertain_external: false,
            completed: true,
        };
        let retried = after_crash_retry(&completed_reconcilable);
        assert!(!retried.uncertain_external);
        assert!(retried.completed);

        let encoded = serde_json::to_string(&retried).unwrap();
        let decoded: ActionOutcome = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, retried);
    }
}
