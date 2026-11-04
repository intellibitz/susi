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
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod mcp_registry_trust_tests {
    use super::*;

    #[test]
    fn mcp_registry_trust_sandboxes_low_scores() {
        let t = trust_score("weird", false, false);
        assert!(t.sandboxed);
        assert!(t.score < 0.5);
        assert!(!trust_score("github", true, true).sandboxed);
    }

    #[test]
    fn trust_score_assigns_expected_tier_for_known_good_unsigned_server() {
        let t = trust_score("local-unsigned", true, false);
        assert_eq!(t.server, "local-unsigned");
        assert!((t.score - 0.7).abs() < 1e-9);
        assert!(t.sandboxed);
    }

    #[test]
    fn trust_score_assigns_expected_tier_for_unknown_signed_server() {
        let t = trust_score("third-party-signed", false, true);
        assert_eq!(t.server, "third-party-signed");
        assert!((t.score - 0.55).abs() < 1e-9);
        assert!(t.sandboxed);
    }

    #[test]
    fn mcp_trust_serde_roundtrip_preserves_server_score_and_sandboxed() {
        let t = trust_score("github", true, true);
        let json = serde_json::to_string(&t).unwrap();
        let decoded: McpTrust = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, t);
    }
}
