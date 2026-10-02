//! Mastery verification for VC-201-064: desired vs observed distinguishes
//! SUSI-owned resources from external resources; operator-selected repair
//! changes only owned fields and reports conflicts without overwriting
//! foreign state.
//!
//! The cited `vc_201_064` tests cover owned-object repair and foreign
//! reporting. The distinguishing property probed here: "only owned fields"
//! must hold even when ownership is leaf-grained — an owned leaf whose
//! observed ancestor is a foreign *scalar* must not be repaired by
//! destroying that scalar.

use crate::managed_drift::{drift, reconcile, DriftKind};
use serde_json::json;
use std::collections::BTreeSet;

fn owned(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|s| s.to_string()).collect()
}

/// An owned leaf beneath a foreign scalar: desired says `a.b = 1`, observed
/// has `a` as an external scalar. `a.b` is reported Missing and owned, but
/// repairing it must refuse — reaching the leaf would require destroying the
/// foreign value at `a`, and the conflict is recorded instead.
#[test]
fn vc_201_064_mastery_owned_leaf_under_foreign_scalar_is_refused() {
    let desired = json!({"a": {"b": 1}});
    let observed = json!({"a": "foreign-scalar"});
    let owned = owned(&["a.b"]);

    let report = drift(&desired, &observed, &owned);
    let item = report.iter().find(|i| i.key == "a.b").unwrap();
    assert_eq!(item.kind, DriftKind::Missing);
    assert!(item.owned, "a.b is owned, so the selection is legitimate");
    // The foreign scalar is visible too: `a` appears as an unowned Extra.
    let ancestor = report.iter().find(|i| i.key == "a").unwrap();
    assert_eq!(ancestor.kind, DriftKind::Extra);
    assert!(!ancestor.owned);

    let res = reconcile(&desired, &observed, &owned, &["a.b".to_string()]);
    assert!(res.applied.is_empty(), "the repair is refused");
    assert_eq!(res.conflicts.len(), 1);
    assert_eq!(res.conflicts[0].key, "a.b");
    assert_eq!(
        res.repaired["a"],
        json!("foreign-scalar"),
        "foreign state is never overwritten"
    );
}

/// Same refusal when the foreign ancestor is an array (arrays are leaves).
#[test]
fn vc_201_064_mastery_owned_leaf_under_foreign_array_is_refused() {
    let desired = json!({"a": {"b": 1}});
    let observed = json!({"a": [1, 2, 3]});
    let owned = owned(&["a.b"]);

    let res = reconcile(&desired, &observed, &owned, &["a.b".to_string()]);
    assert!(res.applied.is_empty());
    assert_eq!(res.conflicts.len(), 1);
    assert_eq!(res.repaired, observed, "foreign array untouched");
}

/// Foreign drift is reported and refused: an external extra field shows up
/// `owned: false` and selecting it yields a conflict, not a mutation.
#[test]
fn vc_201_064_mastery_foreign_drift_reported_but_never_repaired() {
    let desired = json!({"a": 1});
    let observed = json!({"a": 1, "x": "external"});
    let owned = owned(&["a"]);

    let report = drift(&desired, &observed, &owned);
    let item = report.iter().find(|i| i.key == "x").unwrap();
    assert_eq!(item.kind, DriftKind::Extra);
    assert!(!item.owned);

    let res = reconcile(&desired, &observed, &owned, &["x".to_string()]);
    assert!(res.applied.is_empty());
    assert_eq!(res.conflicts.len(), 1);
    assert_eq!(res.conflicts[0].key, "x");
    assert_eq!(res.repaired, observed, "foreign state is byte-identical");
}

/// Foreign changed leaf likewise: reported, refused, untouched.
#[test]
fn vc_201_064_mastery_changed_foreign_leaf_stays() {
    let desired = json!({"a": 1});
    let observed = json!({"a": 2});
    let owned = owned(&[]);

    let report = drift(&desired, &observed, &owned);
    let item = report.iter().find(|i| i.key == "a").unwrap();
    assert_eq!(item.kind, DriftKind::Changed);
    assert!(!item.owned);

    let res = reconcile(&desired, &observed, &owned, &["a".to_string()]);
    assert!(res.applied.is_empty());
    assert_eq!(res.repaired["a"], json!(2));
}

/// Where the claim does hold: owned missing/changed/extra leaves under an
/// owned object repair in both directions, and foreign siblings survive.
#[test]
fn vc_201_064_mastery_owned_repair_preserves_foreign_siblings() {
    let desired = json!({"a": {"keep": 2}, "gone": {"x": 5}});
    let observed = json!({"a": {"keep": 1, "foreign": "ext"}, "gone": {"x": 5, "y": 9}});
    let owned = owned(&["a.keep", "gone.y"]);

    let report = drift(&desired, &observed, &owned);
    let by_key: std::collections::BTreeMap<_, _> =
        report.iter().map(|i| (i.key.as_str(), i)).collect();
    assert_eq!(by_key["a.keep"].kind, DriftKind::Changed);
    assert_eq!(by_key["a.foreign"].kind, DriftKind::Extra);
    assert!(!by_key["a.foreign"].owned);
    assert_eq!(by_key["gone.y"].kind, DriftKind::Extra);
    assert!(by_key["gone.y"].owned);

    let res = reconcile(
        &desired,
        &observed,
        &owned,
        &["a.keep".to_string(), "gone.y".to_string()],
    );
    assert_eq!(
        res.applied,
        vec!["a.keep".to_string(), "gone.y".to_string()]
    );
    assert_eq!(res.repaired["a"]["keep"], json!(2));
    assert_eq!(
        res.repaired["a"]["foreign"],
        json!("ext"),
        "foreign sibling survives owned repair"
    );
    assert!(
        res.repaired["gone"].get("y").is_none(),
        "owned extra leaf removed"
    );
    assert_eq!(res.repaired["gone"]["x"], json!(5));
}

/// No drift: an identical document produces no items, and any operator
/// selection becomes a recorded conflict rather than a forced write.
#[test]
fn vc_201_064_mastery_no_drift_offers_nothing_to_repair() {
    let doc = json!({"a": 1, "b": {"c": [1, 2]}});
    let owned = owned(&["a", "b"]);
    let report = drift(&doc, &doc, &owned);
    assert!(report.is_empty(), "identical state has no repairable work");

    let res = reconcile(&doc, &doc, &owned, &["a".to_string()]);
    assert!(res.applied.is_empty());
    assert_eq!(res.conflicts.len(), 1);
    assert_eq!(res.repaired, doc);
}
