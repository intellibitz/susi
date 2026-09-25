//! Kernel-Mediated Tool Proxy (Swarm OS Bullet 38)
//!
//! Allows cells to securely execute host tools (e.g., git, docker) via a proxy
//! that strictly enforces capability grants through the MAC security module.

use crate::security::CapabilityPolicy;
use std::process::{Command, Output};

/// Error executing a tool via the proxy.
#[derive(Debug, Clone)]
pub enum ToolError {
    Unauthorized(String),
    ExecutionFailed(String),
    InvalidCommand(String),
}

/// The securely mediated tool proxy.
pub struct ToolProxy {
    policy: CapabilityPolicy,
}

impl ToolProxy {
    /// Creates a new tool proxy with the given cell security policy.
    pub fn new(policy: CapabilityPolicy) -> Self {
        Self { policy }
    }

    /// Attempts to execute a host tool on behalf of the cell.
    /// The cell must possess a grant matching `tool:<command>`.
    pub fn execute(&self, command: &str, args: &[String]) -> Result<Output, ToolError> {
        let capability_req = format!("tool:{}", command);

        let mut authorized = false;
        for grant in self.policy.grants() {
            if grant.capability == capability_req || grant.capability == "tool:*" {
                authorized = true;
                break;
            }
        }

        if authorized {
            // Check for obvious escapes (very rudimentary safety check)
            if command.contains("..") || command.contains('/') || command.contains('\\') {
                return Err(ToolError::InvalidCommand(
                    "Path traversal or absolute paths denied".into(),
                ));
            }

            // Execute the tool
            let output = Command::new(command)
                .args(args)
                .output()
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

            Ok(output)
        } else {
            Err(ToolError::Unauthorized(capability_req))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::CapabilityGrant;

    #[test]
    fn test_tool_proxy_enforcement() {
        // Deny-by-default policy
        let empty_policy = CapabilityPolicy::new("test-cell", vec![]);
        let proxy = ToolProxy::new(empty_policy);

        let res = proxy.execute("echo", &[String::from("hello")]);
        assert!(matches!(res, Err(ToolError::Unauthorized(_))));

        // Granted policy
        let granted_policy = CapabilityPolicy::new(
            "test-cell",
            vec![CapabilityGrant {
                capability: "tool:echo".to_string(),
                scope: None,
                ephemeral: false,
            }],
        );
        let proxy2 = ToolProxy::new(granted_policy);

        let res2 = proxy2.execute("echo", &[String::from("hello")]).unwrap();
        let stdout = String::from_utf8_lossy(&res2.stdout);
        assert!(stdout.contains("hello"));

        // Path traversal denial
        let escape_policy = CapabilityPolicy::new(
            "test-cell",
            vec![CapabilityGrant {
                capability: "tool:../bin/bash".to_string(),
                scope: None,
                ephemeral: false,
            }],
        );
        let proxy3 = ToolProxy::new(escape_policy);
        let res3 = proxy3.execute("../bin/bash", &[]);
        assert!(matches!(res3, Err(ToolError::InvalidCommand(_))));
    }
}
