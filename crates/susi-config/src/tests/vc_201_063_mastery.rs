//! Mastery verification for VC-201-063: a deterministic diff with
//! preconditions and operation IDs from the same resolver execution uses;
//! applying a stale plan fails safely and replaying a successful apply does
//! not repeat side effects.
//!
//! The cited `vc_201_063` tests cover the happy path. The distinguishing
//! properties probed here: a stale plan must *fail* for every class of drift
//! (not just changed existing keys), and operation IDs must identify a
//! change, not just a key.

use crate::plan_apply::ConfigStore;
use std::collections::BTreeMap;

fn desired(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Falsification 1: keys absent from the store carry NO precondition. A plan
/// that inserts `new.key` applies even after a concurrent writer sets
/// `new.key`, silently overwriting the interleaved change — a stale plan
/// does not fail safely for insertions.
#[test]
fn vc_201_063_mastery_stale_insertion_plan_overwrites_concurrent_write() {
    let mut store = ConfigStore::default();
    let plan = store.plan(&desired(&[("new.key", "planned")]));
    assert!(
        plan.preconditions.is_empty(),
        "an insertion records no precondition"
    );

    // Concurrent mutation after the plan was produced.
    store.values.insert("new.key".into(), "concurrent".into());

    let applied = store.apply(&plan);
    assert!(
        applied.is_ok(),
        "the stale plan was refused" // documents actual (unsafe) behavior
    );
    assert_eq!(
        store.values.get("new.key").map(String::as_str),
        Some("planned"),
        "the interleaved write was overwritten by the stale plan"
    );
}

/// Falsification 2: operation IDs are `op-<key>` — they name the key, not the
/// change. A second plan that changes the same key is recorded with the same
/// op ID, so apply treats it as a replay and never performs the mutation.
#[test]
fn vc_201_063_mastery_second_change_to_same_key_is_silently_skipped() {
    let mut store = ConfigStore::default();

    let plan_a = store.plan(&desired(&[("k", "a")]));
    assert!(store.apply(&plan_a).is_ok());
    assert_eq!(store.values.get("k").map(String::as_str), Some("a"));

    // A distinct change to the same key produces the same op ID.
    let plan_b = store.plan(&desired(&[("k", "b")]));
    assert_eq!(plan_b.ops[0].id, plan_a.ops[0].id, "op-id is only op-<key>");

    let done = store.apply(&plan_b).unwrap();
    assert_eq!(
        done,
        vec!["skip op-k".to_string()],
        "apply treated the distinct change as a replay"
    );
    assert_eq!(
        store.values.get("k").map(String::as_str),
        Some("a"),
        "the second plan's change never took effect"
    );
}

/// What the claim does get right: plan output is deterministic for a given
/// (values, desired) pair — same ops in sorted key order, same IDs.
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
    let ids: Vec<&str> = p1.ops.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["op-a", "op-b", "op-c"]);
}

/// Precondition safety where preconditions exist: an existing key changed
/// after planning makes apply fail before any mutation.
#[test]
fn vc_201_063_mastery_changed_key_precondition_fails_safely() {
    let mut store = ConfigStore::default();
    store.values.insert("k".into(), "v0".into());
    let plan = store.plan(&desired(&[("k", "v1"), ("other", "x")]));
    assert_eq!(plan.preconditions.get("k").map(String::as_str), Some("v0"));

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

/// Replaying the same plan is a no-op (idempotent) — this part of the claim
/// holds for literal replays of one plan.
#[test]
fn vc_201_063_mastery_literal_replay_repeats_no_side_effects() {
    let mut store = ConfigStore::default();
    let plan = store.plan(&desired(&[("k", "v1")]));
    let first = store.apply(&plan).unwrap();
    let second = store.apply(&plan).unwrap();
    assert_eq!(first, vec!["applied op-k".to_string()]);
    assert_eq!(second, vec!["skip op-k".to_string()]);
    assert_eq!(store.values.get("k").map(String::as_str), Some("v1"));
    assert_eq!(store.values.len(), 1, "replay introduced no extra state");
}
