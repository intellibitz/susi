//! Agent permission requests answered by policy, not interactive prompts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_network: bool,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            allow_read: true,
            allow_write: false,
            allow_network: false,
        }
    }
}

#[must_use]
pub fn decide(policy: &ApprovalPolicy, req: Permission) -> bool {
    match req {
        Permission::Read => policy.allow_read,
        Permission::Write => policy.allow_write,
        Permission::Network => policy.allow_network,
    }
}

#[cfg(test)]
mod zc_agent_approvals_tests {
    use super::*;

    #[test]
    fn zc_agent_approvals_policy_not_prompt() {
        let p = ApprovalPolicy::default();
        assert!(decide(&p, Permission::Read));
        assert!(!decide(&p, Permission::Write));
        assert!(!decide(&p, Permission::Network));
    }
}
