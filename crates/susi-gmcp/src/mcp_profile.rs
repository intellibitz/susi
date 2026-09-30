//! MCP spec versions, negotiation and registry metadata (VC-201-086 / T-CLAUDE-273).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpSpecProfile {
    pub versions: Vec<String>,
    pub negotiated: Option<String>,
    pub registry_metadata: serde_json::Value,
}

impl McpSpecProfile {
    #[must_use]
    pub fn default_profile() -> Self {
        Self {
            versions: vec!["2024-11-05".into(), "2025-03-26".into()],
            negotiated: None,
            registry_metadata: serde_json::json!({
                "registry": "mcp",
                "supports_list_changed": true
            }),
        }
    }

    pub fn negotiate(&mut self, peer_versions: &[&str]) -> Option<String> {
        let chosen = self
            .versions
            .iter()
            .rev()
            .find(|v| peer_versions.iter().any(|p| p == v))
            .cloned();
        self.negotiated = chosen.clone();
        chosen
    }
}

#[cfg(test)]
mod eco_mcp_versions_tests {
    use super::*;

    fn load_fixture(name: &str) -> serde_json::Value {
        let path = format!("../../../config/ecosystem/{name}");
        let text = std::fs::read_to_string(path).unwrap_or_else(|_| {
            // also try from CARGO_MANIFEST_DIR
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../config/ecosystem")
                .join(name);
            std::fs::read_to_string(p).expect("ecosystem fixture")
        });
        serde_json::from_str(&text).expect("json")
    }

    #[test]
    fn eco_mcp_versions_fixture_lists_revisions_with_provenance() {
        let mcp = load_fixture("mcp.json");
        assert_eq!(mcp["entity"]["id"], "mcp");
        assert_eq!(mcp["entity"]["kind"], "protocol");
        let v = load_fixture("mcp-2025-03-26.json");
        assert_eq!(v["entity"]["version"], "2025-03-26");
        let p = &v["entity"]["provenance"];
        assert!(p["source"]
            .as_str()
            .unwrap()
            .contains("modelcontextprotocol"));
        assert!(!p["retrieved"].as_str().unwrap().is_empty());
        assert_eq!(p["confidence"], "verified");
    }

    #[test]
    fn eco_mcp_versions_negotiate_initialize_picks_common() {
        let mut p = McpSpecProfile::default_profile();
        assert_eq!(
            p.negotiate(&["2024-11-05", "2025-03-26"]).as_deref(),
            Some("2025-03-26")
        );
        let supersedes = load_fixture("mcp-2025-03-26.json");
        assert!(supersedes["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "supersedes"));
    }
}

#[cfg(test)]
mod eco_mcp_transports_tests {
    fn load(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn eco_mcp_transports_documents_stdio_http_and_legacy_sse() {
        let p = load("mcp-transports.json");
        let methods: Vec<_> = p["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["method"].as_str().unwrap())
            .collect();
        assert!(methods.contains(&"STDIO"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"GET+SSE"));
        assert!(p["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "session-resume"));
        assert_eq!(p["provenance"]["confidence"], "verified");
    }
}

#[cfg(test)]
mod eco_mcp_features_tests {
    fn load(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn eco_mcp_features_lists_server_and_client_capabilities() {
        let p = load("mcp-features.json");
        let caps: Vec<_> = p["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        for need in [
            "tools",
            "resources",
            "prompts",
            "sampling",
            "roots",
            "elicitation",
        ] {
            assert!(caps.contains(&need), "missing {need}");
        }
        assert!(p["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["method"] == "tools/call"));
    }
}
