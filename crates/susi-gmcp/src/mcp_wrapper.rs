use crate::susi_core::registry::{CapabilityRegistry, Tool};
use crate::tool_registry::GmcpClient;
pub struct McpDynamicTool {
    pub server_name: String,
    pub registry_name: String,
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
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let args_str = if let Some(s) = args.as_str() {
            s.to_string()
        } else {
            args.to_string()
        };
        GmcpClient::execute_external_tool_result(&self.server_name, &self.mcp_tool_name, &args_str)
    }
}
pub fn register_mcp_servers() {
    let registry = CapabilityRegistry::global();
    let live = GmcpClient::discover_live_tools();
    let fallback = if live.is_empty() {
        GmcpClient::list_external_tools()
    } else {
        Vec::new()
    };
    register_discovered_tools(registry, live, fallback);
}
pub(crate) fn register_discovered_tools(
    registry: &CapabilityRegistry,
    live: Vec<(String, crate::tool_types::McpTool)>,
    fallback: Vec<crate::tool_types::McpTool>,
) {
    if live.is_empty() {
        for t in fallback {
            let registry_name = t.name.clone();
            let (server_name, mcp_tool_name) = match registry_name.split_once(':') {
                Some((server, tool)) => (server.to_string(), tool.to_string()),
                None => continue,
            };
            let tool = McpDynamicTool {
                server_name,
                registry_name,
                mcp_tool_name,
                description: t.description,
            };
            if registry.get_tool(tool.name()).is_none() {
                registry.register_tool(tool);
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
/// Pillar 8 entrypoint; alias of [`register_mcp_servers`].
pub fn auto_discover_mcp() {
    register_mcp_servers();
}
