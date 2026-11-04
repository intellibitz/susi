//! MCP server and client features profile (tools, resources, prompts,
//! sampling, elicitation, roots, logging, completion): offline contract tests
//! over `config/ecosystem/profiles/mcp-features.json`. The GMCP requirements
//! ledger (`.agents/gmcp/requirements-ledger.json`) names this file as the
//! production path of its MCP-features rows.

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

#[test]
fn eco_mcp_features_declares_profile_metadata_and_verified_provenance() {
    let p = load("mcp-features.json");
    assert_eq!(p["version"], "eco-profile/v1");
    assert_eq!(p["id"], "mcp-features");
    assert_eq!(p["subject"], "mcp");
    assert_eq!(p["provenance"]["confidence"], "verified");
}

#[test]
fn eco_mcp_features_tracks_current_and_deprecated_protocol_versions() {
    let p = load("mcp-features.json");
    let versions = p["versions"].as_array().unwrap();
    assert!(versions
        .iter()
        .any(|v| v["id"] == "2025-06-18" && v["status"] == "current"));
    assert!(versions
        .iter()
        .any(|v| v["id"] == "2024-11-05" && v["status"] == "deprecated"));
}

#[test]
fn eco_mcp_features_defines_request_response_shapes_and_non_retriable_errors() {
    let p = load("mcp-features.json");
    let req_names: Vec<_> = p["request_shapes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(req_names.contains(&"json-rpc"));
    assert!(req_names.contains(&"server-features"));
    assert!(req_names.contains(&"client-features"));

    let errors = p["errors"].as_array().unwrap();
    assert!(errors
        .iter()
        .any(|e| e["code"] == "-32700" && e["retriable"] == false));
    assert!(errors
        .iter()
        .any(|e| e["code"] == "-32601" && e["retriable"] == false));
}

#[test]
fn eco_mcp_features_includes_logging_and_completion_in_server_feature_fields_and_response_shapes() {
    let p = load("mcp-features.json");
    let server_shape = p["request_shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "server-features")
        .unwrap();
    let fields: Vec<_> = server_shape["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(fields.contains(&"logging/setLevel"));
    assert!(fields.contains(&"completion/complete"));

    let resp_names: Vec<_> = p["response_shapes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(resp_names.contains(&"json-rpc-result"));
    assert!(resp_names.contains(&"negotiation"));
}
