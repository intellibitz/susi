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
}
