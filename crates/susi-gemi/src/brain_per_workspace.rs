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

    /// Verify structural invariants on per-workspace evidence mappings.
    pub fn verify_workspace_brain_invariants(&self) -> Result<(), String> {
        for (workspace, evidence_ids) in &self.by_workspace {
            if workspace.trim().is_empty() {
                return Err("WorkspaceBrain workspace key must not be empty".to_string());
            }
            if evidence_ids.is_empty() {
                return Err(format!(
                    "WorkspaceBrain evidence list for '{workspace}' must not be empty"
                ));
            }
            for id in evidence_ids {
                if id.trim().is_empty() {
                    return Err(format!(
                        "WorkspaceBrain evidence id in '{workspace}' must not be empty"
                    ));
                }
            }
        }
        Ok(())
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

    #[test]
    fn verify_brain_wave13_invariants_workspace_brain() {
        let mut valid = WorkspaceBrain::default();
        valid.record("/workspace/a", "EV-1");
        assert!(valid.verify_workspace_brain_invariants().is_ok());

        let blank_ws = WorkspaceBrain {
            by_workspace: BTreeMap::from([("   ".to_string(), vec!["EV-1".to_string()])]),
        };
        assert!(blank_ws.verify_workspace_brain_invariants().is_err());

        let empty_list = WorkspaceBrain {
            by_workspace: BTreeMap::from([("/workspace/a".to_string(), vec![])]),
        };
        assert!(empty_list.verify_workspace_brain_invariants().is_err());

        let blank_id = WorkspaceBrain {
            by_workspace: BTreeMap::from([("/workspace/a".to_string(), vec!["  ".to_string()])]),
        };
        assert!(blank_id.verify_workspace_brain_invariants().is_err());
    }
}
