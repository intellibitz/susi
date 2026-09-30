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
