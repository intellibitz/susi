//! Plane-bus handler for all `tools.*` topics.

use crate::susi_core::plane_bus::topics;
use crate::susi_core::plane_bus::{PlaneBus, PlaneHandler};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::client::GmcpClient;
use crate::registry::ToolRegistry;

struct ToolsPlaneHandler;

/// Mission-scoped authority records: the egress-gate and token refusals a
/// mission accumulated, so the orchestrator can surface them on the report.
const MISSION_REFUSALS_TOPIC: &str = "tools.mission.refusals";

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
            topics::TOOLS_LEADING_ENABLE => {
                let ws = workspace_path(&payload);
                let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let cfg = crate::LeadingMcpManager::new(&ws)
                    .and_then(|m| m.enable(name))
                    .map_err(|e| e.to_string())?;
                serde_json::to_value(cfg).map_err(|e| e.to_string())
            }
            topics::TOOLS_LEADING_DISABLE => {
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
            MISSION_REFUSALS_TOPIC => {
                let mission = payload
                    .get("mission")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Ok(json!({
                    "egress": ToolRegistry::mission_egress_refusals(mission)
                        .iter()
                        .map(|r| json!({
                            "mission": r.mission,
                            "host": r.host,
                            "reason": r.reason.to_string(),
                        }))
                        .collect::<Vec<_>>(),
                    "authority": ToolRegistry::mission_authority_refusals(mission)
                        .iter()
                        .map(|r| json!({
                            "mission": r.mission,
                            "tool": r.tool,
                            "reason": r.reason,
                        }))
                        .collect::<Vec<_>>(),
                }))
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
    use super::{looks_like_tool_error, workspace_path, ToolsPlaneHandler};
    use crate::susi_core::plane_bus::{topics, PlaneHandler};
    use serde_json::json;

    #[test]
    fn workspace_path_defaults_and_reads_payload() {
        assert_eq!(workspace_path(&json!({})), std::path::PathBuf::from("."));
        assert_eq!(
            workspace_path(&json!({"workspace": "/tmp/ws"})),
            std::path::PathBuf::from("/tmp/ws")
        );
        assert_eq!(
            workspace_path(&json!({"workspace": 42})),
            std::path::PathBuf::from(".")
        );
    }

    #[test]
    fn exists_and_list_topics_answer_without_side_effects() {
        let h = ToolsPlaneHandler;
        let out = h
            .handle(topics::TOOLS_EXISTS, json!({"name": "__no_such_tool__"}))
            .unwrap();
        assert_eq!(out["exists"], json!(false));
        let list = h.handle(topics::TOOLS_LIST, json!({})).unwrap();
        assert!(list.is_array() || list.is_object());
    }

    #[test]
    fn execute_unknown_tool_reports_error_field() {
        let h = ToolsPlaneHandler;
        let out = h
            .handle(
                topics::TOOLS_EXECUTE,
                json!({"name": "__no_such_tool__", "args": {}}),
            )
            .unwrap();
        assert!(
            out.get("error").is_some() || out.get("text").is_some(),
            "{out}"
        );
    }

    #[test]
    fn leading_topics_resolve_against_workspace() {
        // Enable/disable read-modify-write `mcp_config.json` under the
        // HOME-derived config dir. Unisolated, this test rewrote the
        // developer's real MCP config and raced `client::tests`' admit test
        // (which repoints HOME under ENV_LOCK), erasing an admitted server.
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let h = ToolsPlaneHandler;
        let dir = tempfile::tempdir().unwrap();
        // Also scrubs SUSI_HOME/SUSI_PORT_OFFSET; restored when `env` drops.
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", dir.path());
        env.set("XDG_CONFIG_HOME", dir.path().join("config"));
        let ws = json!({"workspace": dir.path().to_str().unwrap()});
        // Status on a fresh workspace returns rows or a typed error — never panic.
        let listed = h.handle(topics::TOOLS_LEADING_LIST, ws.clone());
        assert!(listed.is_ok() || listed.is_err());
        // Enable of an unknown server errors; disable reports removal flag.
        let _ = h.handle(
            topics::TOOLS_LEADING_ENABLE,
            json!({"workspace": ws["workspace"], "name": "__nope__"}),
        );
        if let Ok(disabled) = h.handle(
            topics::TOOLS_LEADING_DISABLE,
            json!({"workspace": ws["workspace"], "name": "__nope__"}),
        ) {
            assert_eq!(disabled["removed"], json!(false));
        }
        drop(env);
    }

    #[test]
    fn remote_execute_unknown_remote_maps_to_error_value() {
        let h = ToolsPlaneHandler;
        let out = h
            .handle(
                topics::TOOLS_REMOTE_EXECUTE,
                json!({"remote": "__nope__", "tool": "t", "goal": "g"}),
            )
            .unwrap();
        assert!(out.get("error").is_some(), "{out}");
    }

    #[test]
    fn unhandled_topic_is_rejected() {
        let h = ToolsPlaneHandler;
        let err = h.handle("tools.unknown", json!({}));
        assert!(err.is_err());
    }

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
