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

#[test]
fn default_profile_and_negotiate_no_overlap_leave_negotiated_none() {
    let mut p = McpSpecProfile::default_profile();
    assert!(p.negotiated.is_none());
    assert_eq!(p.registry_metadata["probe_status"], "unprobed");
    assert!(p.negotiate(&["1999-01-01"]).is_none());
    assert!(p.negotiated.is_none());
}

#[test]
fn record_probe_updates_negotiated_version_and_registry_metadata() {
    use crate::mcp_probe::McpProbeCheck;
    use std::collections::BTreeMap;

    let mut checks = BTreeMap::new();
    checks.insert(
        "initialize".to_string(),
        McpProbeCheck {
            passed: true,
            detail: "passed".to_string(),
        },
    );
    let report = McpProbeReport {
        protocol_version: Some("2025-03-26".to_string()),
        server_info: Some(serde_json::json!({"name": "srv", "version": "1.0"})),
        capabilities: Some(serde_json::json!({})),
        supports_list_changed: Some(true),
        tools: vec!["tool_a".to_string()],
        checks,
    };
    let mut profile = McpSpecProfile::default_profile();
    profile.record_probe(&report);
    assert_eq!(profile.negotiated.as_deref(), Some("2025-03-26"));
    assert_eq!(profile.registry_metadata["probe_status"], "passed");
    assert_eq!(profile.registry_metadata["supports_list_changed"], true);
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
