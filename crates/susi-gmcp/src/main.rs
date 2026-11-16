#![forbid(unsafe_code)]

use susi_gmcp::susi_abi::cell::cell_ports;
use susi_gmcp::susi_abi::swarm::SwarmRole;
use susi_gmcp::susi_abi::syscall::{SyscallRequest, SyscallResponse};

use susi_gmcp::susi_abi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    susi_abi::cell_server::run_pillar(
        "susi-gmcp",
        cell_ports::GMCP,
        "gmcp-tools-01",
        SwarmRole::ToolDriver,
        &["mcp-protocol", "system-tools"],
        susi_gmcp::susi_config::SusiConfig::ensure_api_auth_token_seeded,
        handle_tool,
    )
}

fn handle_tool(req: SyscallRequest) -> SyscallResponse {
    // Dispatch through the same plane-bus tool path as the daemon. When no
    // tools handler is registered in this process the call fails and the
    // caller gets an Error status - never a fabricated success.
    let name = req
        .payload
        .get("name")
        .or_else(|| req.payload.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args = req
        .payload
        .get("args")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let workspace = std::path::PathBuf::from(req.workspace.as_deref().unwrap_or("."));
    let started = std::time::Instant::now();
    let outcome = if name.is_empty() {
        Err("missing tool name in payload (`name`)".to_string())
    } else {
        // Runs on this connection's own thread; the cell server answers a
        // panic in the tool as an `Error`.
        susi_gmcp::plane_tools::execute_tool(&name, &args, &workspace).map_err(|e| e.to_string())
    };
    let latency_us = susi_abi::cell_server::elapsed_micros(started);
    // The tool registry flattens failures (EaiError display, capability
    // gaps, reflex errors) into text; never report those as a success.
    let outcome = outcome.and_then(|text| {
        if looks_like_tool_failure(&text) {
            Err(text)
        } else {
            Ok(text)
        }
    });
    let (data, error) = match outcome {
        Ok(text) => (serde_json::json!({ "result":text }), None),
        Err(message) => (serde_json::Value::Null, Some(message)),
    };
    susi_abi::cell_server::handler_reply(req.id, latency_us, data, error)
}

/// True when tool output is a flattened failure rather than a result:
/// registry markers anywhere, or an `EaiError`-style display near the head
/// (same rule as `GemiEngine::looks_like_error_text`).
fn looks_like_tool_failure(text: &str) -> bool {
    let t = text.trim_start();
    // A JSON document is a result, never a flattened error: a report row's
    // `detail` (sorted first) can quote an `... Error:` line near the head.
    if (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(t).is_ok()
    {
        return false;
    }
    if t.starts_with("[CAPABILITY_GAP]")
        || t.starts_with("[RECOVERY]")
        || t.starts_with("[FAIL]")
        || t.starts_with("Reflex Error:")
    {
        return true;
    }
    let head: String = t.chars().take(64).collect();
    head.contains(" Error:") || head.contains(" Violation:")
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use susi_gmcp::susi_abi::syscall::SyscallOp;

    #[test]
    fn classifies_flattened_tool_failures() {
        assert!(looks_like_tool_failure(
            "[CAPABILITY_GAP] Tool 'x' missing from Meta-Substrate."
        ));
        assert!(looks_like_tool_failure("Reflex Error: invalid reflex name"));
        assert!(looks_like_tool_failure(
            "Sandbox Error: path escapes workspace"
        ));
        assert!(looks_like_tool_failure("Governance Violation: denied"));
        assert!(!looks_like_tool_failure("SUSI Engine Version: 0.14.1"));
        assert!(!looks_like_tool_failure(
            "Wrote 2048 bytes to notes/incident-review.txt; the file quotes an old Sandbox Error: line"
        ));
    }

    #[test]
    fn looks_like_tool_failure_treats_valid_json_documents_as_non_failures() {
        assert!(!looks_like_tool_failure(
            r#"{"detail":"Sandbox Error: quoted in JSON report"}"#
        ));
        assert!(!looks_like_tool_failure(
            r#"[{"msg":"Governance Violation: quoted"}]"#
        ));
    }

    #[test]
    fn handle_tool_rejects_missing_tool_name_with_error_status() {
        let _env_lock = susi_gmcp::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", dir.path())
            .set("XDG_CONFIG_HOME", dir.path().join(".config"))
            .set("XDG_DATA_HOME", dir.path().join(".local").join("share"))
            .set("XDG_CACHE_HOME", dir.path().join(".cache"))
            .set("SUSI_HOME", dir.path().join(".susi"));
        let req = SyscallRequest {
            id: "tool-1".to_string(),
            caller_id: "caller".to_string(),
            op: SyscallOp::ToolCall,
            token: None,
            workspace: Some(dir.path().display().to_string()),
            payload: serde_json::json!({}),
            timestamp: 1,
        };
        let resp = handle_tool(req);
        assert_eq!(resp.id, "tool-1");
        assert_eq!(resp.status, susi_abi::syscall::SyscallStatus::Error);
        assert!(resp
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("missing tool name"));
    }

    #[test]
    fn tool_driver_swarm_cell_registers_mcp_and_system_tools_capabilities() {
        let bind_addr = susi_abi::cell::cell_bind_addr(cell_ports::GMCP);
        let mut cell = susi_abi::cell::SwarmCell::new(
            "gmcp-tools-01".to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{bind_addr}"),
        );
        cell.register_capability("mcp-protocol");
        cell.register_capability("system-tools");
        assert_eq!(cell.manifest.role, SwarmRole::ToolDriver);
        assert_eq!(cell.manifest.capabilities.len(), 2);
    }

    #[test]
    fn handle_tool_accepts_tool_alias_and_preserves_request_id() {
        let _env_lock = susi_gmcp::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", dir.path())
            .set("XDG_CONFIG_HOME", dir.path().join(".config"))
            .set("XDG_DATA_HOME", dir.path().join(".local").join("share"))
            .set("XDG_CACHE_HOME", dir.path().join(".cache"))
            .set("SUSI_HOME", dir.path().join(".susi"));
        let req = SyscallRequest {
            id: "tool-alias-2".to_string(),
            caller_id: "caller".to_string(),
            op: SyscallOp::ToolCall,
            token: None,
            workspace: Some(dir.path().display().to_string()),
            payload: serde_json::json!({
                "tool": "unregistered_tool_xyz",
                "args": {}
            }),
            timestamp: 2,
        };
        let resp = handle_tool(req);
        assert_eq!(resp.id, "tool-alias-2");
    }
}
