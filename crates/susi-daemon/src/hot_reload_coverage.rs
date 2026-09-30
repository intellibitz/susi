//! Prove config hot-reload without daemon restart.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotReloadEvent {
    pub key: String,
    pub applied: bool,
}

/// Apply a config delta in-process; returns which keys changed.
#[must_use]
pub fn apply_hot_reload(
    current: &mut BTreeMap<String, String>,
    delta: BTreeMap<String, String>,
) -> Vec<HotReloadEvent> {
    let mut events = Vec::new();
    for (k, v) in delta {
        let changed = current.get(&k).map(|c| c != &v).unwrap_or(true);
        current.insert(k.clone(), v);
        events.push(HotReloadEvent {
            key: k,
            applied: changed,
        });
    }
    events
}

#[cfg(test)]
mod hot_reload_coverage_tests {
    use super::*;

    #[test]
    fn hot_reload_coverage_applies_without_restart() {
        let mut cfg = BTreeMap::from([("port".into(), "9090".into())]);
        let ev = apply_hot_reload(
            &mut cfg,
            BTreeMap::from([
                ("port".into(), "9190".into()),
                ("theme".into(), "dark".into()),
            ]),
        );
        assert_eq!(cfg.get("port").map(String::as_str), Some("9190"));
        assert!(ev.iter().all(|e| e.applied));
    }
}
