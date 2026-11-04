use crate::eval_contamination::detect;
use std::collections::BTreeSet;

#[test]
fn vc_201_009_clean_run_not_contaminated() {
    let held = BTreeSet::from(["h1".into(), "h2".into()]);
    let train = BTreeSet::from(["t1".into()]);
    let mem = BTreeSet::from(["m1".into()]);
    let r = detect(&held, &train, &mem);
    assert!(!r.contaminated);
    assert!(r.reason.is_none());
}

#[test]
fn vc_201_009_overlap_excludes_with_reason() {
    let held = BTreeSet::from(["h1".into()]);
    let train = BTreeSet::from(["h1".into(), "t2".into()]);
    let mem = BTreeSet::new();
    let r = detect(&held, &train, &mem);
    assert!(r.contaminated);
    assert!(r.reason.as_deref().unwrap().contains("h1"));
}

#[test]
fn vc_201_009_memory_overlap_with_heldout_flags_contamination() {
    let held = BTreeSet::from(["eval-item-7".into()]);
    let train = BTreeSet::from(["train-1".into()]);
    let mem = BTreeSet::from(["eval-item-7".into(), "mem-2".into()]);
    let r = detect(&held, &train, &mem);
    assert!(r.contaminated);
    assert!(r.reason.as_deref().unwrap().contains("eval-item-7"));
}

#[test]
fn vc_201_009_empty_heldout_set_is_never_contaminated() {
    let held = BTreeSet::new();
    let train = BTreeSet::from(["t1".into(), "t2".into()]);
    let mem = BTreeSet::from(["t1".into(), "m1".into()]);
    let r = detect(&held, &train, &mem);
    assert!(!r.contaminated);
    assert!(r.reason.is_none());
}
