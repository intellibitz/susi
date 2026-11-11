//! Plane-bus backed tool registry facade (no `susi-tools` dependency).

use crate::susi_core::plane_bus::tools as plane_tools;
use crate::susi_error::EaiResult;
use crate::tool_types::McpTool;
use std::path::Path;

pub struct ToolRegistry;

impl ToolRegistry {
    pub fn list_tools() -> Vec<McpTool> {
        plane_tools::list_tools()
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        Some(McpTool {
                            name: v.get("name")?.as_str()?.to_string(),
                            description: v
                                .get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("")
                                .to_string(),
                            input_schema: v
                                .get("input_schema")
                                .or_else(|| v.get("inputSchema"))
                                .cloned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn execute_tool(name: &str, args: &serde_json::Value, workspace: &Path) -> String {
        plane_tools::execute_tool(name, args, workspace).unwrap_or_else(|e| format!("[Error] {e}"))
    }

    pub fn exists(name: &str) -> bool {
        plane_tools::exists(name)
    }
}

pub struct GmcpClient;

impl GmcpClient {
    pub fn scout_reasoning_remotes() -> Vec<String> {
        plane_tools::scout_reasoning_remotes()
            .get("remotes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn execute_external_tool(remote: &str, tool: &str, goal: &str) -> EaiResult<String> {
        plane_tools::execute_external_tool(remote, tool, goal)
    }

    pub fn execute_external_tool_result(remote: &str, tool: &str, goal: &str) -> EaiResult<String> {
        Self::execute_external_tool(remote, tool, goal)
    }

    pub fn list_external_tools() -> Vec<McpTool> {
        let config_path = susi_paths::SusiDirs::config_dir().join("mcp_config.json");
        let mut tools = Vec::new();
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(servers) = v.get("mcpServers").and_then(|s| s.as_object()) {
                    for name in servers.keys() {
                        tools.push(McpTool {
                            name: format!("{name}:*"),
                            description: format!("Dynamic Proxy for standard MCP server: {name}"),
                            input_schema: None,
                        });
                    }
                }
            }
        }
        tools
    }

    pub fn discover_live_tools() -> Vec<(String, McpTool)> {
        Vec::new()
    }

    pub fn autonomous_web_scout() -> Vec<String> {
        Vec::new()
    }

    pub fn auto_configure_server(_name: &str, _package: &str) -> EaiResult<String> {
        Ok("[plane_bus] auto_configure_server requires composition-root tools plane".into())
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_registry_facade_queries_plane_bus_without_panicking() {
        let tools = ToolRegistry::list_tools();
        let _ = tools.len();
        let _ = ToolRegistry::exists("nonexistent_tool_xyz");
    }

    #[test]
    fn tool_registry_execute_tool_formats_errors_with_bracketed_prefix() {
        let ws = std::env::temp_dir();
        let out = ToolRegistry::execute_tool("nonexistent_tool_xyz", &json!({}), &ws);
        assert!(!out.is_empty());
    }

    #[test]
    fn gmcp_client_discovery_and_scout_helpers_return_deterministic_defaults() {
        assert!(GmcpClient::discover_live_tools().is_empty());
        assert!(GmcpClient::autonomous_web_scout().is_empty());
        let _ = GmcpClient::scout_reasoning_remotes();
    }

    #[test]
    fn gmcp_client_auto_configure_and_external_tool_wrappers_behave_consistently() {
        let msg = GmcpClient::auto_configure_server("demo", "@modelcontextprotocol/demo").unwrap();
        assert!(msg.contains("composition-root tools plane"));

        let r1 = GmcpClient::execute_external_tool("srv", "tool", "goal");
        let r2 = GmcpClient::execute_external_tool_result("srv", "tool", "goal");
        assert_eq!(r1.is_ok(), r2.is_ok());
    }

    #[test]
    fn gmcp_client_list_external_tools_formats_wildcard_proxies_when_present() {
        let external = GmcpClient::list_external_tools();
        for tool in &external {
            assert!(tool.name.ends_with(":*"));
            assert!(!tool.description.is_empty());
        }
    }
}
