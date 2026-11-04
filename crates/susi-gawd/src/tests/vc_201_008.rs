use crate::intent_invariants::{action_in_allowlist, metamorphic_whitespace};

#[test]
fn vc_201_008_metamorphic_whitespace_holds() {
    let inv = metamorphic_whitespace(|s| format!("ACTION: {s}"), " list ", "list");
    assert!(inv.holds);
    assert!(inv.counterexample.is_none());
}

#[test]
fn vc_201_008_counterexample_reproduces() {
    let inv = metamorphic_whitespace(|s| s.to_string(), "a", "b");
    assert!(!inv.holds);
    let cx = inv.counterexample.expect("cx");
    assert!(cx.contains("a") && cx.contains("b"));
}

#[test]
fn vc_201_008_allowlist_rejects_unknown() {
    assert!(action_in_allowlist("list_dirs", &["list_dirs", "status"]).holds);
    assert!(!action_in_allowlist("rm_rf", &["list_dirs"]).holds);
}

#[test]
fn vc_201_008_allowlist_counterexample_names_rejected_action_and_empty_allowlist() {
    let allowed = action_in_allowlist("status", &["list_dirs", "status"]);
    assert_eq!(allowed.name, "allowlist");
    assert!(allowed.holds);
    assert!(allowed.counterexample.is_none());

    let rejected = action_in_allowlist("drop_table", &[]);
    assert_eq!(rejected.name, "allowlist");
    assert!(!rejected.holds);
    assert_eq!(rejected.counterexample.as_deref(), Some("drop_table"));
}
