use susi_core::registry::{CapabilityRegistry, Tool};
use susi_tools::GmcpClient;

/// Wraps an MCP tool as a dynamically executable Tool.
pub struct McpDynamicTool {
    pub server_name: String,
    pub tool_name: String,
    pub description: String,
}

impl Tool for McpDynamicTool {
    fn name(&self) -> &str {
        &self.tool_name
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
            GmcpClient::execute_external_tool(&self.server_name, &self.tool_name, &args_str);
        Ok(result)
    }
}

pub fn register_mcp_servers(registry: &CapabilityRegistry) {
    let tools = GmcpClient::list_external_tools();
    for t in tools {
        // e.g., brave_search:brave_web_search
        let parts: Vec<&str> = t.name.splitn(2, ':').collect();
        if parts.len() == 2 {
            let tool = McpDynamicTool {
                server_name: parts[0].to_string(),
                tool_name: t.name.clone(), // or parts[1].to_string()
                description: t.description,
            };
            registry.register_tool(tool);
        }
    }
}
