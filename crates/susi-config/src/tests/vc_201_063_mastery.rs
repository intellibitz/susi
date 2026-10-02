//! Mastery verification for VC-201-063: a deterministic diff with
//! preconditions and operation IDs from the same resolver execution uses;
//! applying a stale plan fails safely and replaying a successful apply does
//! not repeat side effects.
//!
//! Falsification found two real gaps, now fixed: keys absent at plan time
//! carried no precondition (a stale insert overwrote a concurrent write),
//! and op IDs named the key (`op-<key>`) rather than the change (a second,
//! distinct change to a key was skipped as a replay). These tests pin the
//! invariants, not the bugs.

use crate::plan_apply::ConfigStore;
use std::collections::BTreeMap;

fn desired(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Absence is a precondition: a plan that inserts `new.key` must refuse to
/// apply after a concurrent writer set `new.key`.
#[test]
fn vc_201_063_mastery_stale_insertion_plan_fails_safely() {
    let mut store = ConfigStore::default();
    let plan = store.plan(&desired(&[("new.key", "planned")]));
    assert_eq!(
        plan.preconditions.get("new.key"),
        Some(&None),
        "an insertion records a must-stay-absent precondition"
    );

    // Concurrent mutation after the plan was produced.
    store.values.insert("new.key".into(), "concurrent".into());

    assert!(
        store.apply(&plan).is_err(),
        "a stale plan must fail, not overwrite the interleaved write"
    );
    assert_eq!(
        store.values.get("new.key").map(String::as_str),
        Some("concurrent"),
        "the interleaved write survives the refused apply"
    );
}

/// Operation IDs name the change: a second plan changing the same key gets a
/// different ID and actually applies — it is not mistaken for a replay.
#[test]
fn vc_201_063_mastery_second_change_to_same_key_applies() {
    let mut store = ConfigStore::default();

    let plan_a = store.plan(&desired(&[("k", "a")]));
    assert!(store.apply(&plan_a).is_ok());
    assert_eq!(store.values.get("k").map(String::as_str), Some("a"));

    // A distinct change to the same key gets a distinct op ID.
    let plan_b = store.plan(&desired(&[("k", "b")]));
    assert_ne!(
        plan_b.ops[0].id, plan_a.ops[0].id,
        "op-id must differ for a different (from, to) pair"
    );

    let done = store.apply(&plan_b).unwrap();
    assert!(
        done.iter().any(|l| l.starts_with("applied")),
        "the second change applied, it was not skipped: {done:?}"
    );
    assert_eq!(store.values.get("k").map(String::as_str), Some("b"));
}

/// Plan output is deterministic for a given (values, desired) pair — same
/// ops in sorted key order, same content-derived IDs.
#[test]
fn vc_201_063_mastery_plan_is_deterministic_for_identical_inputs() {
    let mut store = ConfigStore::default();
    store.values.insert("a".into(), "old".into());
    let want = desired(&[("a", "new"), ("b", "1"), ("c", "2")]);
    let p1 = store.plan(&want);
    let p2 = store.plan(&want);
    assert_eq!(p1, p2);
    let keys: Vec<&str> = p1.ops.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(keys, ["a", "b", "c"], "ops iterate the BTreeMap in order");
    assert!(
        p1.ops
            .iter()
            .all(|o| o.id.starts_with("op-") && o.id.len() > 3),
        "ids are stable and non-empty"
    );
}

/// Precondition safety for existing keys: a value changed after planning
/// makes apply fail before any mutation.
#[test]
fn vc_201_063_mastery_changed_key_precondition_fails_safely() {
    let mut store = ConfigStore::default();
    store.values.insert("k".into(), "v0".into());
    let plan = store.plan(&desired(&[("k", "v1"), ("other", "x")]));
    assert_eq!(
        plan.preconditions.get("k").and_then(|o| o.as_deref()),
        Some("v0")
    );

    store.values.insert("k".into(), "concurrent".into());
    assert!(
        store.apply(&plan).is_err(),
        "a stale plan over an existing key is refused"
    );
    assert_eq!(
        store.values.get("k").map(String::as_str),
        Some("concurrent"),
        "the concurrent value survived the refused apply"
    );
    assert!(
        !store.values.contains_key("other"),
        "no partial mutation leaked before the precondition check"
    );
}

/// Replaying the same plan is a no-op (idempotent).
#[test]
fn vc_201_063_mastery_literal_replay_repeats_no_side_effects() {
    let mut store = ConfigStore::default();
    let plan = store.plan(&desired(&[("k", "v1")]));
    let first = store.apply(&plan).unwrap();
    let second = store.apply(&plan).unwrap();
    assert!(first.iter().all(|l| l.starts_with("applied")));
    assert!(second.iter().all(|l| l.starts_with("skip")));
    assert_eq!(store.values.get("k").map(String::as_str), Some("v1"));
    assert_eq!(store.values.len(), 1, "replay introduced no extra state");
}
