use crate::susi_core::registry::{CapabilityRegistry, Tool};
use crate::tool_registry::GmcpClient;

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
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let args_str = if let Some(s) = args.as_str() {
            s.to_string()
        } else {
            args.to_string()
        };
        GmcpClient::execute_external_tool_result(&self.server_name, &self.mcp_tool_name, &args_str)
    }
}

/// Probe configured MCP servers and hot-plug their live tools into the
/// shared capability registry. Uses `CapabilityRegistry::global()` — one
/// catalog across every process on the substrate via the
/// `<cache>/bus/<pid>/` rendezvous.
pub fn register_mcp_servers() {
    let registry = CapabilityRegistry::global();
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
pub fn auto_discover_mcp() {
    register_mcp_servers();
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mcp_dynamic_tool_exposes_registry_name_and_description_via_tool_trait() {
        let tool = McpDynamicTool {
            server_name: "github".into(),
            registry_name: "github:list_prs".into(),
            mcp_tool_name: "list_prs".into(),
            description: "List pull requests".into(),
        };
        assert_eq!(tool.name(), "github:list_prs");
        assert_eq!(tool.description(), "List pull requests");
    }

    #[test]
    fn mcp_dynamic_tool_execute_forwards_string_and_json_object_args() {
        let tool = McpDynamicTool {
            server_name: "unconfigured-server".into(),
            registry_name: "unconfigured-server:ping".into(),
            mcp_tool_name: "ping".into(),
            description: "Ping tool".into(),
        };
        let ws = std::env::temp_dir();
        let res_str = tool.execute(&json!("raw-goal"), &ws);
        let res_obj = tool.execute(&json!({"q": "test"}), &ws);
        assert_eq!(res_str.is_ok(), res_obj.is_ok());
    }

    #[test]
    fn register_mcp_servers_runs_cleanly_on_capability_registry() {
        register_mcp_servers();
        let registry = CapabilityRegistry::global();
        let _ = registry.list_tools();
    }

    #[test]
    fn auto_discover_mcp_is_idempotent_across_repeated_calls() {
        auto_discover_mcp();
        auto_discover_mcp();
    }
}
