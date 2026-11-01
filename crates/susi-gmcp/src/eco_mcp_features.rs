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
