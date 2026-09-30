//! MCP server discovery with trust scoring and sandboxing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTrust {
    pub server: String,
    pub score: f64,
    pub sandboxed: bool,
}

/// Score an MCP server; unknown/untrusted hosts get sandboxing.
#[must_use]
pub fn trust_score(server: &str, known_good: bool, signed: bool) -> McpTrust {
    let score = if known_good && signed {
        0.95
    } else if known_good {
        0.7
    } else if signed {
        0.55
    } else {
        0.2
    };
    McpTrust {
        server: server.into(),
        score,
        sandboxed: score < 0.8,
    }
}

#[cfg(test)]
mod mcp_registry_trust_tests {
    use super::*;

    #[test]
    fn mcp_registry_trust_sandboxes_low_scores() {
        let t = trust_score("weird", false, false);
        assert!(t.sandboxed);
        assert!(t.score < 0.5);
        assert!(!trust_score("github", true, true).sandboxed);
    }
}
