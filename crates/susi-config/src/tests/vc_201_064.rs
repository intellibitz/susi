//! Managed configuration drift detection and reconcile (VC-201-064).

use crate::managed_drift::{drift, reconcile, DriftKind};
use serde_json::json;
use std::collections::BTreeSet;

fn owned() -> BTreeSet<String> {
    ["providers", "models.default"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn vc_201_064_drift_distinguishes_owned_from_external() {
    let desired = json!({
        "providers": {"openai": {"model": "gpt-5"}},
        "models": {"default": "local"},
        "foreign": {"tool": "x"}
    });
    let observed = json!({
        "providers": {"openai": {"model": "gpt-4"}},
        "models": {"default": "local"},
        "foreign": {"tool": "x"}
    });
    let items = drift(&desired, &observed, &owned());
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.key, "providers.openai.model");
    assert_eq!(item.kind, DriftKind::Changed);
    assert!(item.owned);
    // The foreign-managed `foreign.tool` key is desired+observed equal here;
    // a foreign key that *differs* must surface as unowned, not repairable.
}

#[test]
fn vc_201_064_foreign_fields_are_reported_but_never_repairable() {
    let desired = json!({"providers": {"a": 1}, "external": {"zone": "us"}});
    let observed = json!({"providers": {"a": 1}, "external": {"zone": "eu"}});
    let items = drift(&desired, &observed, &owned());
    let ext = items.iter().find(|i| i.key == "external.zone").unwrap();
    assert!(!ext.owned, "external.* is not SUSI-owned");
    let report = reconcile(&desired, &observed, &owned(), &["external.zone".into()]);
    assert!(report.applied.is_empty());
    assert_eq!(report.conflicts.len(), 1);
    // Foreign state untouched.
    assert_eq!(report.repaired["external"]["zone"], json!("eu"));
}

#[test]
fn vc_201_064_selected_repair_changes_only_owned_leaves() {
    let desired = json!({"providers": {"openai": {"model": "gpt-5", "key": "k"}}, "ext": 1});
    let observed = json!({"providers": {"openai": {"model": "gpt-4", "key": "k"}}, "ext": 2});
    let report = reconcile(
        &desired,
        &observed,
        &owned(),
        &["providers.openai.model".into(), "ext".into()],
    );
    assert_eq!(report.applied, vec!["providers.openai.model".to_string()]);
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(
        report.repaired["providers"]["openai"]["model"],
        json!("gpt-5")
    );
    assert_eq!(report.repaired["providers"]["openai"]["key"], json!("k"));
    assert_eq!(report.repaired["ext"], json!(2));
}

#[test]
fn vc_201_064_missing_and_extra_leaves_repair_in_both_directions() {
    let owned: BTreeSet<String> = ["a"].iter().map(|s| s.to_string()).collect();
    let desired = json!({"a": {"want": 1}});
    let observed = json!({"a": {"stale": 9}});
    let items = drift(&desired, &observed, &owned);
    assert!(items
        .iter()
        .any(|i| i.key == "a.want" && i.kind == DriftKind::Missing));
    assert!(items
        .iter()
        .any(|i| i.key == "a.stale" && i.kind == DriftKind::Extra));
    let report = reconcile(
        &desired,
        &observed,
        &owned,
        &["a.want".into(), "a.stale".into()],
    );
    assert_eq!(report.applied.len(), 2);
    assert_eq!(report.repaired["a"], json!({"want": 1}));
}

#[test]
fn vc_201_064_no_drift_means_nothing_to_repair() {
    let doc = json!({"providers": {"a": 1}});
    assert!(drift(&doc, &doc, &owned()).is_empty());
    let report = reconcile(&doc, &doc, &owned(), &["providers.a".into()]);
    assert_eq!(report.conflicts.len(), 1, "no drift item to repair");
}
