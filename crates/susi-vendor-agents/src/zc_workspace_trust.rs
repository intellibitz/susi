//! Workspace trust decided once, not per flag.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTrust {
    trusted: BTreeSet<String>,
}

impl WorkspaceTrust {
    pub fn trust_once(&mut self, path: &str) {
        self.trusted.insert(path.to_string());
    }

    #[must_use]
    pub fn is_trusted(&self, path: &str) -> bool {
        self.trusted.contains(path)
    }
}

#[cfg(test)]
mod zc_workspace_trust_tests {
    use super::*;

    #[test]
    fn zc_workspace_trust_decided_once() {
        let mut t = WorkspaceTrust::default();
        assert!(!t.is_trusted("/repo"));
        t.trust_once("/repo");
        assert!(t.is_trusted("/repo"));
    }
}
