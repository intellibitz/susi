//! Managed configuration drift detection and reconcile (VC-201-064).
//!
//! Desired vs. observed state is compared at leaf granularity. Fields SUSI
//! does not own are external state: they are reported but never repairable.
//! Operator-selected repair rewrites only owned leaf paths and reports
//! conflicts instead of overwriting foreign state.

use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// Owned by SUSI, absent in observed state.
    Missing,
    /// Owned by SUSI, present with a different value.
    Changed,
    /// Owned by SUSI, present in observed but not in desired.
    Extra,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriftItem {
    /// Dotted leaf path, e.g. `providers.openai.model`.
    pub key: String,
    pub kind: DriftKind,
    /// True when the path is SUSI-owned and therefore repairable.
    pub owned: bool,
    pub desired: Option<Value>,
    pub observed: Option<Value>,
}

fn is_owned(owned: &BTreeSet<String>, path: &str) -> bool {
    // A path is owned when it or an ancestor is listed — owning
    // `providers` owns `providers.openai.model`.
    let mut p = path;
    loop {
        if owned.contains(p) {
            return true;
        }
        let Some((parent, _)) = p.rsplit_once('.') else {
            return false;
        };
        p = parent;
    }
}

fn walk(v: &Value, path: &str, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(map) => {
            if map.is_empty() {
                out.push((path.to_string(), v.clone()));
            }
            for (k, val) in map {
                walk(val, &format!("{path}.{k}"), out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            out.push((path.to_string(), v.clone()));
        }
    }
}

fn leaves(v: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out.into_iter()
        .map(|(p, val)| (p.trim_start_matches('.').to_string(), val))
        .collect()
}

/// Compare desired and observed documents. Foreign-owned fields appear in
/// the report marked `owned: false` so an operator sees the whole picture,
/// but they can never be selected for repair.
#[must_use]
pub fn drift(desired: &Value, observed: &Value, owned: &BTreeSet<String>) -> Vec<DriftItem> {
    let desired_leaves: std::collections::BTreeMap<String, Value> =
        leaves(desired).into_iter().collect();
    let observed_leaves: std::collections::BTreeMap<String, Value> =
        leaves(observed).into_iter().collect();
    let mut items = Vec::new();
    for (key, dv) in &desired_leaves {
        match observed_leaves.get(key) {
            None => items.push(DriftItem {
                key: key.clone(),
                kind: DriftKind::Missing,
                owned: is_owned(owned, key),
                desired: Some(dv.clone()),
                observed: None,
            }),
            Some(ov) if ov != dv => items.push(DriftItem {
                key: key.clone(),
                kind: DriftKind::Changed,
                owned: is_owned(owned, key),
                desired: Some(dv.clone()),
                observed: Some(ov.clone()),
            }),
            _ => {}
        }
    }
    for (key, ov) in &observed_leaves {
        if !desired_leaves.contains_key(key) {
            items.push(DriftItem {
                key: key.clone(),
                kind: DriftKind::Extra,
                owned: is_owned(owned, key),
                desired: None,
                observed: Some(ov.clone()),
            });
        }
    }
    items
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepairReport {
    /// Leaf paths rewritten to their desired value.
    pub applied: Vec<String>,
    /// Selections refused: unowned paths, or paths with no drift.
    pub conflicts: Vec<DriftItem>,
    /// Observed document after repair; foreign state is byte-identical.
    pub repaired: Value,
}

fn set_leaf(doc: &mut Value, path: &str, value: &Value) {
    let mut node = &mut *doc;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if !node.is_object() {
            *node = Value::Object(serde_json::Map::new());
        }
        let Value::Object(map) = node else {
            return;
        };
        node = if parts.peek().is_none() {
            map.insert(part.to_string(), value.clone());
            return;
        } else {
            map.entry(part.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
        };
    }
}

/// Apply the operator's selected drift items to a copy of `observed`.
/// Only owned paths change; each refused selection is recorded as a
/// conflict rather than silently dropped or forced through.
pub fn reconcile(
    desired: &Value,
    observed: &Value,
    owned: &BTreeSet<String>,
    selected: &[String],
) -> RepairReport {
    let report = drift(desired, observed, owned);
    let mut repaired = observed.clone();
    let mut applied = Vec::new();
    let mut conflicts = Vec::new();
    for sel in selected {
        match report.iter().find(|i| &i.key == sel) {
            Some(item) if item.owned => {
                match item.kind {
                    DriftKind::Missing | DriftKind::Changed => {
                        if let Some(v) = &item.desired {
                            set_leaf(&mut repaired, sel, v);
                            applied.push(sel.clone());
                        }
                    }
                    DriftKind::Extra => {
                        // Desired has no value for this leaf: removing
                        // owned extras is allowed — delete the leaf.
                        remove_leaf(&mut repaired, sel);
                        applied.push(sel.clone());
                    }
                }
            }
            Some(item) => conflicts.push(item.clone()),
            None => conflicts.push(DriftItem {
                key: sel.clone(),
                kind: DriftKind::Changed,
                owned: is_owned(owned, sel),
                desired: None,
                observed: None,
            }),
        }
    }
    RepairReport {
        applied,
        conflicts,
        repaired,
    }
}

fn remove_leaf(doc: &mut Value, path: &str) {
    let mut node = doc;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if let Value::Object(map) = node {
                map.remove(part);
            }
            return;
        }
        match node.as_object_mut().and_then(|m| m.get_mut(part)) {
            Some(next) => node = next,
            None => return,
        }
    }
}
