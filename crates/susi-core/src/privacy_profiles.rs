//! Per-workspace privacy posture profiles with egress allowlists.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyProfile {
    pub allowlist: Vec<String>,
    pub local_only: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyProfiles {
    pub by_workspace: BTreeMap<String, PrivacyProfile>,
}

impl PrivacyProfiles {
    pub fn set(&mut self, workspace: &str, profile: PrivacyProfile) {
        self.by_workspace.insert(workspace.into(), profile);
    }

    #[must_use]
    pub fn allows(&self, workspace: &str, host: &str) -> bool {
        match self.by_workspace.get(workspace) {
            Some(p) if p.local_only => false,
            Some(p) => p.allowlist.iter().any(|h| h == host),
            None => false,
        }
    }
}

#[cfg(test)]
mod privacy_profiles_tests {
    use super::*;

    #[test]
    fn privacy_profiles_per_workspace_allowlist() {
        let mut p = PrivacyProfiles::default();
        p.set(
            "/ws",
            PrivacyProfile {
                allowlist: vec!["api.openai.com".into()],
                local_only: false,
            },
        );
        assert!(p.allows("/ws", "api.openai.com"));
        assert!(!p.allows("/ws", "evil.example"));
        assert!(!p.allows("/other", "api.openai.com"));
    }
}
