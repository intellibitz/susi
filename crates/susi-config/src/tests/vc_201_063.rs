use crate::plan_apply::ConfigStore;
use std::collections::BTreeMap;

#[test]
fn vc_201_063_deterministic_diff_with_op_ids() {
    let mut s = ConfigStore::default();
    s.values.insert("a".into(), "1".into());
    let desired = BTreeMap::from([("a".into(), "2".into()), ("b".into(), "9".into())]);
    let plan = s.plan(&desired);
    assert_eq!(plan.ops.len(), 2);
    assert!(plan.ops.iter().all(|o| !o.id.is_empty()));
    assert_eq!(
        plan.preconditions.get("a").and_then(|o| o.as_deref()),
        Some("1")
    );
}

#[test]
fn vc_201_063_stale_plan_fails_safely() {
    let mut s = ConfigStore::default();
    s.values.insert("a".into(), "1".into());
    let plan = s.plan(&BTreeMap::from([("a".into(), "2".into())]));
    s.values.insert("a".into(), "x".into());
    assert!(s.apply(&plan).unwrap_err().contains("stale"));
}

#[test]
fn vc_201_063_replay_is_idempotent() {
    let mut s = ConfigStore::default();
    let plan = s.plan(&BTreeMap::from([("a".into(), "2".into())]));
    let r1 = s.apply(&plan).unwrap();
    let r2 = s.apply(&plan).unwrap();
    assert!(r1.iter().any(|l| l.starts_with("applied")));
    assert!(r2.iter().all(|l| l.starts_with("skip")));
    assert_eq!(s.values.get("a").map(String::as_str), Some("2"));
}
