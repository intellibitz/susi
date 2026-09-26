//! Plane-bus handler for all `tools.*` topics.

use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::client::GmcpClient;
use crate::registry::ToolRegistry;

struct ToolsPlaneHandler;

fn workspace_path(payload: &Value) -> PathBuf {
    payload
        .get("workspace")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

impl PlaneHandler for ToolsPlaneHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        match topic {
            topics::TOOLS_EXISTS => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                Ok(json!({ "exists": ToolRegistry::exists(name) }))
            }
            topics::TOOLS_EXECUTE => {
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = payload.get("args").cloned().unwrap_or(json!({}));
                let ws = workspace_path(&payload);
                let text = ToolRegistry::execute_tool(name, &args, &ws);
                if looks_like_tool_error(&text) {
                    Ok(json!({ "error": text }))
                } else {
                    Ok(json!({ "text": text }))
                }
            }
            topics::TOOLS_AUTO_LINK => {
                ToolRegistry::auto_link_essential_mcp_servers();
                Ok(json!({ "ok": true }))
            }
            topics::TOOLS_LIST => {
                let tools = ToolRegistry::list_tools();
                serde_json::to_value(tools).map_err(|e| e.to_string())
            }
            topics::TOOLS_LEADING_LIST => {
                let ws = workspace_path(&payload);
                let rows = crate::LeadingMcpManager::new(&ws)
                    .and_then(|m| m.status())
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(rows).map_err(|e| e.to_string())
            }
            topics::TOOLS_LEADING_GET => {
                let ws = workspace_path(&payload);
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let cfg = crate::LeadingMcpManager::new(&ws)
                    .and_then(|m| m.enable(name))
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(cfg).map_err(|e| e.to_string())
            }
            topics::TOOLS_LEADING_REMOVE => {
                let ws = workspace_path(&payload);
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let removed = crate::LeadingMcpManager::new(&ws)
                    .and_then(|m| m.disable(name))
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "removed": removed }))
            }
            topics::TOOLS_SCOUT_REMOTES => {
                let remotes = GmcpClient::scout_reasoning_remotes();
                Ok(json!({ "remotes": remotes }))
            }
            topics::TOOLS_REMOTE_EXECUTE => {
                let remote = payload.get("remote").and_then(|v| v.as_str()).unwrap_or("");
                let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                let goal = payload.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                match GmcpClient::execute_external_tool_result(remote, tool, goal) {
                    Ok(text) => Ok(json!({ "text": text })),
                    Err(e) => Ok(json!({ "error": e.to_string() })),
                }
            }
            other => Err(format!("tools handler: unhandled topic '{other}'")),
        }
    }
}

/// `ToolRegistry::execute_tool` flattens `Err(EaiError)` and MCP/JSON-RPC
/// failures into bare strings, so error results must be detected textually —
/// otherwise `"Protocol Error: ..."` leaks to callers as successful output.
/// Display prefixes are checked only near the head so content that merely
/// mentions an error is not misclassified.
fn looks_like_tool_error(text: &str) -> bool {
    let t = text.trim_start();
    if t.starts_with("[FAIL]") || (t.starts_with('[') && t.contains("Error")) {
        return true;
    }
    let head: String = t.chars().take(64).collect();
    head.contains(" Error:")
        || head.contains(" Violation:")
        || head.contains("Mcp error")
        || head.contains("MCP Error")
}

pub fn register() {
    PlaneBus::global().register_prefix("tools.", Arc::new(ToolsPlaneHandler));
}

#[cfg(test)]
mod tests {
    use super::looks_like_tool_error;

    #[test]
    fn tool_error_text_detection_covers_stringified_failures() {
        // The observed live leak: an MCP -32603 flattened to bare text.
        assert!(looks_like_tool_error(
            "Protocol Error: Mcp error: -32603: Unknown tool: reason"
        ));
        assert!(looks_like_tool_error("[FAIL] MCP: connection refused"));
        assert!(looks_like_tool_error("[Tool Error] missing arg"));
        assert!(looks_like_tool_error("Sandbox Error: path denied"));
        assert!(looks_like_tool_error("Governance Violation: blocked"));
        // Legitimate output must not be misclassified.
        assert!(!looks_like_tool_error("the answer is 42"));
        assert!(!looks_like_tool_error(
            "Errors are documented later in this report."
        ));
        assert!(!looks_like_tool_error(""));
    }
}
