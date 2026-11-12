use crate::mcp_wrapper::{
    auto_discover_mcp, register_discovered_tools, register_mcp_servers, McpDynamicTool,
};
use crate::susi_core::registry::Tool;
use crate::tool_registry::{discover_live_tools_with, GmcpClient, ToolRegistry};
use serde_json::json;
use std::sync::Arc;

#[test]
fn tool_registry_facade_queries_plane_bus_without_panicking() {
    let _ = ToolRegistry::list_tools().len();
    let _ = ToolRegistry::exists("nonexistent_tool_xyz");
    assert!(
        !ToolRegistry::execute_tool("nonexistent_tool_xyz", &json!({}), &std::env::temp_dir())
            .is_empty()
    );
}

#[test]
fn live_discovery_uses_the_commercial_shape_without_a_transport_dependency() {
    let request: Arc<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync> =
        Arc::new(|payload| {
            assert_eq!(payload["config"]["command"], "demo");
            Ok(
                json!({"tools": [{"name": "search", "description": "Find a document", "inputSchema": {"type": "object"}}]}),
            )
        });
    let tools =
        discover_live_tools_with(vec![("demo".into(), json!({"command": "demo"}))], request);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0, "demo");
    assert_eq!(tools[0].1.name, "demo:search");
    assert_eq!(tools[0].1.input_schema, Some(json!({"type": "object"})));
}

#[test]
fn gmcp_client_rejects_incomplete_configuration_before_bus_access() {
    assert!(GmcpClient::auto_configure_server("", "pkg").is_err());
    assert!(GmcpClient::auto_configure_server("demo", "").is_err());
}

#[test]
fn mcp_dynamic_tool_preserves_proxy_identity() {
    let tool = McpDynamicTool {
        server_name: "filesystem".into(),
        registry_name: "filesystem:read_text".into(),
        mcp_tool_name: "read_text".into(),
        description: "Read text file via MCP".into(),
    };
    assert_eq!(tool.name(), "filesystem:read_text");
    assert_eq!(tool.description(), "Read text file via MCP");
}

#[test]
fn discovered_live_tools_register_with_server_and_bare_name() {
    let registry = crate::susi_core::registry::CapabilityRegistry::new();
    register_discovered_tools(
        &registry,
        vec![(
            "demo".into(),
            crate::tool_types::McpTool {
                name: "demo:search".into(),
                description: "Search documents".into(),
                input_schema: None,
            },
        )],
        Vec::new(),
    );
    let tool = registry
        .get_tool("demo:search")
        .expect("live MCP tool is registered");
    assert_eq!(tool.name(), "demo:search");
}

#[test]
fn wrapper_entrypoints_are_safe_to_repeat() {
    register_mcp_servers();
    auto_discover_mcp();
    auto_discover_mcp();
}
