use crate::susi_core::plane_bus::{self, tools as plane_tools};
use crate::susi_error::{EaiError, EaiResult};
use crate::tool_types::McpTool;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
const TOPIC_MCP_LIST_TOOLS: &str = "ecosystem_hub.mcp.list_tools";
const DISCOVERY_BUDGET: Duration = Duration::from_secs(12);
const LEASE_MS: u64 = 10_000;
const HANDSHAKE_MS: u64 = 5_000;
pub struct ToolRegistry;
fn mcp_tool(name: String, value: &Value) -> McpTool {
    McpTool {
        name,
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        input_schema: value
            .get("input_schema")
            .or_else(|| value.get("inputSchema"))
            .cloned(),
    }
}
fn budgeted(mut payload: Value) -> Value {
    payload["lease_ms"] = json!(LEASE_MS);
    payload["handshake_ms"] = json!(HANDSHAKE_MS);
    payload
}
impl ToolRegistry {
    pub fn list_tools() -> Vec<McpTool> {
        plane_tools::list_tools()
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .filter_map(|value| Some(mcp_tool(value.get("name")?.as_str()?.into(), value)))
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
            .and_then(Value::as_array)
            .map(|remotes| {
                remotes
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
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
        configured_servers()
            .into_iter()
            .map(|(name, _)| McpTool {
                name: format!("{name}:*"),
                description: format!("Dynamic Proxy for standard MCP server: {name}"),
                input_schema: None,
            })
            .collect()
    }
    pub fn discover_live_tools() -> Vec<(String, McpTool)> {
        let servers = configured_servers();
        let request: Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync> =
            Arc::new(|payload| {
                plane_bus::PlaneBus::global().request(TOPIC_MCP_LIST_TOOLS, payload)
            });
        discover_live_tools_with(servers, request)
    }
    pub fn autonomous_web_scout() -> Vec<String> {
        let live = Self::discover_live_tools();
        if live.is_empty() {
            return Self::list_external_tools()
                .into_iter()
                .map(|tool| format!("{} — {} (not probed)", tool.name, tool.description))
                .collect();
        }
        live.into_iter()
            .map(|(_, tool)| format!("{} — {}", tool.name, tool.description))
            .collect()
    }
    pub fn auto_configure_server(name: &str, package: &str) -> EaiResult<String> {
        let (name, package) = (name.trim(), package.trim());
        if name.is_empty() || package.is_empty() {
            return Err(EaiError::protocol(
                "MCP server name and package are required",
            ));
        }
        let (command_or_url, args) =
            if package.starts_with("http://") || package.starts_with("https://") {
                (package.to_string(), Vec::new())
            } else {
                ("npx".into(), vec!["-y".into(), package.to_string()])
            };
        plane_bus::daemon::adapters(json!({
            "action": "mcp_add", "name": name, "command_or_url": command_or_url, "args": args
        }))
        .map_err(EaiError::process)?
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| EaiError::protocol("MCP adapter returned no admission message"))
    }
}
fn read_config() -> Option<Value> {
    std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("mcp_config.json"))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}
fn configured_servers() -> Vec<(String, Value)> {
    read_config()
        .and_then(|value| {
            value
                .get("mcpServers")
                .or_else(|| value.get("mcp_servers"))
                .and_then(Value::as_object)
                .map(|servers| {
                    servers
                        .iter()
                        .map(|(name, config)| (name.clone(), config.clone()))
                        .collect()
                })
        })
        .unwrap_or_default()
}
pub(crate) fn discover_live_tools_with(
    servers: Vec<(String, Value)>,
    request: Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>,
) -> Vec<(String, McpTool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    for (server_name, config) in servers {
        let (tx, request) = (tx.clone(), Arc::clone(&request));
        let _ = std::thread::Builder::new()
            .name("mcp-probe".into())
            .spawn(move || {
                let result = request(budgeted(json!({"config": config})));
                let _ = tx.send((server_name, result));
            });
    }
    drop(tx);
    let deadline = Instant::now() + DISCOVERY_BUDGET;
    let mut discovered = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok((server_name, Ok(reply))) = rx.recv_timeout(remaining) else {
            break;
        };
        let Some(tools) = reply.get("tools").and_then(Value::as_array) else {
            continue;
        };
        discovered.extend(tools.iter().filter_map(|tool| {
            let name = tool.get("name")?.as_str()?;
            Some((
                server_name.clone(),
                mcp_tool(format!("{server_name}:{name}"), tool),
            ))
        }));
    }
    discovered
}
