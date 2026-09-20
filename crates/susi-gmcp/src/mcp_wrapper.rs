use susi_core::registry::{CapabilityRegistry, Tool};
use susi_tools::GmcpClient;

/// Wraps an MCP tool as a dynamically executable Tool.
pub struct McpDynamicTool {
    pub server_name: String,
    /// Registry / swarm-facing name (`server:tool`).
    pub registry_name: String,
    /// Bare MCP tool name passed to the remote server.
    pub mcp_tool_name: String,
    pub description: String,
}

impl Tool for McpDynamicTool {
    fn name(&self) -> &str {
        &self.registry_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn execute(
        &self,
        args: &serde_json::Value,
        _workspace: &std::path::Path,
    ) -> susi_error::EaiResult<String> {
        let args_str = if let Some(s) = args.as_str() {
            s.to_string()
        } else {
            args.to_string()
        };
        let result =
            GmcpClient::execute_external_tool(&self.server_name, &self.mcp_tool_name, &args_str);
        Ok(result)
    }
}

/// Probe configured MCP servers and hot-plug their live tools into the registry.
pub fn register_mcp_servers(registry: &CapabilityRegistry) {
    let live = GmcpClient::discover_live_tools();
    if live.is_empty() {
        // Config-only fallback: advertise wildcard proxies so swarm routing
        // still knows the server exists when live probing is unavailable.
        for t in GmcpClient::list_external_tools() {
            let parts: Vec<&str> = t.name.splitn(2, ':').collect();
            if parts.len() == 2 {
                let tool = McpDynamicTool {
                    server_name: parts[0].to_string(),
                    registry_name: t.name.clone(),
                    mcp_tool_name: parts[1].to_string(),
                    description: t.description,
                };
                if registry.get_tool(tool.name()).is_none() {
                    registry.register_tool(tool);
                }
            }
        }
        return;
    }

    for (server_name, t) in live {
        let mcp_tool_name = t
            .name
            .split_once(':')
            .map(|(_, tool)| tool.to_string())
            .unwrap_or_else(|| t.name.clone());
        let tool = McpDynamicTool {
            server_name,
            registry_name: t.name.clone(),
            mcp_tool_name,
            description: t.description,
        };
        if registry.get_tool(tool.name()).is_none() {
            registry.register_tool(tool);
        }
    }
}

/// Pillar 8 entrypoint: discover configured MCP servers and hot-plug them
/// into the capability registry. Alias of [`register_mcp_servers`].
pub fn auto_discover_mcp(registry: &CapabilityRegistry) {
    register_mcp_servers(registry);
}
