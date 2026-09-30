//! Agent CLI flags derived without hand-edited config.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFlags {
    pub auto_approve_safe: bool,
    pub workspace_trust: bool,
}

impl Default for AgentFlags {
    fn default() -> Self {
        Self {
            auto_approve_safe: true,
            workspace_trust: true,
        }
    }
}

#[cfg(test)]
mod zc_agent_flags_tests {
    use super::*;

    #[test]
    fn zc_agent_flags_defaults_are_safe() {
        let f = AgentFlags::default();
        assert!(f.auto_approve_safe);
        assert!(f.workspace_trust);
    }
}
