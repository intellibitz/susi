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

    #[test]
    fn hot_reload_identical_value_reports_applied_false() {
        let mut cfg = BTreeMap::from([("port".into(), "9090".into())]);
        let ev = apply_hot_reload(&mut cfg, BTreeMap::from([("port".into(), "9090".into())]));
        assert_eq!(
            ev,
            vec![HotReloadEvent {
                key: "port".into(),
                applied: false,
            }]
        );
    }

    #[test]
    fn hot_reload_empty_delta_returns_empty_events_and_preserves_current() {
        let mut cfg = BTreeMap::from([("mode".into(), "strict".into())]);
        let ev = apply_hot_reload(&mut cfg, BTreeMap::new());
        assert!(ev.is_empty());
        assert_eq!(cfg.get("mode").map(String::as_str), Some("strict"));
    }

    #[test]
    fn hot_reload_mixed_changed_and_unchanged_keys_reports_per_key_applied() {
        let mut cfg = BTreeMap::from([("alpha".into(), "1".into()), ("beta".into(), "2".into())]);
        let ev = apply_hot_reload(
            &mut cfg,
            BTreeMap::from([
                ("alpha".into(), "1".into()),
                ("beta".into(), "22".into()),
                ("gamma".into(), "3".into()),
            ]),
        );
        assert_eq!(
            ev,
            vec![
                HotReloadEvent {
                    key: "alpha".into(),
                    applied: false,
                },
                HotReloadEvent {
                    key: "beta".into(),
                    applied: true,
                },
                HotReloadEvent {
                    key: "gamma".into(),
                    applied: true,
                },
            ]
        );
    }

    #[test]
    fn hot_reload_event_serde_roundtrip_preserves_key_and_applied_flag() {
        let ev = HotReloadEvent {
            key: "log_level".into(),
            applied: true,
        };
        let encoded = serde_json::to_string(&ev).expect("serialize HotReloadEvent");
        let decoded: HotReloadEvent =
            serde_json::from_str(&encoded).expect("deserialize HotReloadEvent");
        assert_eq!(decoded, ev);
    }
}
