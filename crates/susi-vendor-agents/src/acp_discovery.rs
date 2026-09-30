//! Discover installed ACP agents in the ecosystem scan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpAgent {
    pub id: String,
    pub path: String,
}

/// Detect ACP agents from binary names present on PATH / known dirs.
#[must_use]
pub fn discover_acp(binaries: &[&str]) -> Vec<AcpAgent> {
    binaries
        .iter()
        .filter(|b| {
            let n = b.to_ascii_lowercase();
            n.contains("acp") || n.ends_with("-agent") || n.contains("agent-client")
        })
        .map(|b| AcpAgent {
            id: (*b).into(),
            path: format!("/usr/local/bin/{b}"),
        })
        .collect()
}

#[cfg(test)]
mod acp_discovery_tests {
    use super::*;

    #[test]
    fn acp_discovery_finds_agent_binaries() {
        let found = discover_acp(&["cargo", "acp-bridge", "my-agent", "ls"]);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|a| a.id == "acp-bridge"));
    }
}
