//! MCP authorization profile: OAuth 2.1 with protected-resource metadata
//! (RFC 9728), resource indicators (RFC 8707) and metadata discovery. Offline
//! contract tests over `config/ecosystem/profiles/mcp-authorization.json`.
//! The GMCP requirements ledger (`.agents/gmcp/requirements-ledger.json`)
//! names this file as the production path of its MCP-authorization rows.

fn load(name: &str) -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/ecosystem/profiles")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// Every field of every request (or response) shape, as one list.
fn fields(profile: &serde_json::Value, shapes: &str) -> Vec<String> {
    profile[shapes]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["fields"].as_array().unwrap().iter())
        .map(|f| f.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn eco_mcp_auth_profile_is_the_mcp_authorization_format() {
    let p = load("mcp-authorization.json");
    assert_eq!(p["version"], "eco-profile/v1");
    assert_eq!(p["id"], "mcp-authorization");
    assert_eq!(p["subject"], "mcp");
    assert!(p["versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["id"] == "2025-06-18" && v["status"] == "current"));
}

#[test]
fn eco_mcp_auth_covers_discovery_and_resource_indicators() {
    let p = load("mcp-authorization.json");
    let requests = fields(&p, "request_shapes");
    // RFC 9728: the server publishes its authorization server.
    assert!(requests
        .iter()
        .any(|f| f.contains("/.well-known/oauth-protected-resource")));
    // RFC 8707: the token is bound to the MCP server it is for.
    assert!(requests.iter().any(|f| f.contains("resource=")));
    assert!(requests.iter().any(|f| f.contains("PKCE")));
}

#[test]
fn eco_mcp_auth_rejects_bad_tokens_with_a_challenge() {
    let p = load("mcp-authorization.json");
    let responses = fields(&p, "response_shapes");
    for need in ["401 invalid_token", "403 insufficient_scope"] {
        assert!(responses.iter().any(|f| f == need), "{need}");
    }
    assert!(responses.iter().any(|f| f.contains("WWW-Authenticate")));
}
