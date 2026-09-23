//! Plane-bus backed tool registry facade (no `susi-tools` dependency).

use crate::tool_types::McpTool;
use std::path::Path;
use susi_core::plane_bus::tools as plane_tools;
use susi_error::EaiResult;

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
