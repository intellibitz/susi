//! Wiring: side-effect classify/reconcile in MissionDag dispatch (T-INTELLIBITZ-7).

use crate::dag::MissionDag;
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
