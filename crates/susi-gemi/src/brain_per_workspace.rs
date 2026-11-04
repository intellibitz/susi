//! Per-workspace brain evidence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceBrain {
    pub by_workspace: BTreeMap<String, Vec<String>>,
}

impl WorkspaceBrain {
    pub fn record(&mut self, workspace: &str, evidence_id: &str) {
        self.by_workspace
            .entry(workspace.into())
            .or_default()
            .push(evidence_id.into());
    }

    #[must_use]
    pub fn for_workspace(&self, workspace: &str) -> Vec<String> {
        self.by_workspace
            .get(workspace)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod brain_per_workspace_tests {
    use super::*;

    #[test]
    fn brain_per_workspace_isolates_evidence() {
        let mut b = WorkspaceBrain::default();
        b.record("/a", "EV-1");
        b.record("/b", "EV-2");
        assert_eq!(b.for_workspace("/a"), vec!["EV-1".to_string()]);
        assert!(b.for_workspace("/c").is_empty());
    }

    #[test]
    fn record_preserves_insertion_order_within_same_workspace() {
        let mut b = WorkspaceBrain::default();
        b.record("/ws", "EV-first");
        b.record("/ws", "EV-second");
        b.record("/ws", "EV-third");
        assert_eq!(
            b.for_workspace("/ws"),
            vec![
                "EV-first".to_string(),
                "EV-second".to_string(),
                "EV-third".to_string()
            ]
        );
    }

    #[test]
    fn by_workspace_maintains_deterministic_btreemap_key_ordering() {
        let mut b = WorkspaceBrain::default();
        b.record("/zeta", "EV-z");
        b.record("/alpha", "EV-a");
        b.record("/beta", "EV-b");
        let keys: Vec<&str> = b.by_workspace.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["/alpha", "/beta", "/zeta"]);
    }

    #[test]
    fn workspace_brain_serde_json_roundtrip() {
        let mut b = WorkspaceBrain::default();
        b.record("/repo-1", "EV-10");
        b.record("/repo-2", "EV-20");
        let encoded = serde_json::to_string(&b).expect("serialize WorkspaceBrain");
        let decoded: WorkspaceBrain =
            serde_json::from_str(&encoded).expect("deserialize WorkspaceBrain");
        assert_eq!(decoded, b);
    }

    #[test]
    fn default_workspace_brain_is_empty_and_unknown_workspace_returns_empty_vec() {
        let b = WorkspaceBrain::default();
        assert!(b.by_workspace.is_empty());
        assert!(b.for_workspace("/unknown").is_empty());
    }
}
