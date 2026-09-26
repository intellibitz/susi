#![forbid(unsafe_code)]

use std::sync::Arc;

use susi_gmcp::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gmcp::susi_abi::swarm::SwarmRole;
use susi_gmcp::susi_abi::syscall::{
    SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};

use susi_gmcp::susi_abi;

// Shared swarm-cell server loop (canonical: crates/susi-abi/src/cell_server.rs).
#[rustfmt::skip]
#[path = "../../susi-abi/src/cell_server.rs"]
mod cell_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = cell_bind_addr(cell_ports::GMCP);
    println!("susi-gmcp micro-daemon starting on {bind_addr}");

    let mut cell = SwarmCell::new(
        "gmcp-tools-01".to_string(),
        SwarmRole::ToolDriver,
        format!("tcp://{bind_addr}"),
    );

    // Register capabilities
    cell.register_capability("mcp-protocol");
    cell.register_capability("system-tools");

    // Every syscall must present the host API token (or an explicit
    // SUSI_CELL_TOKEN override); heartbeats stay open for liveness.
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(susi_gmcp::susi_config::SusiConfig::ensure_api_auth_token_seeded)
        .into();

    cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "susi-gmcp Swarm Cell",
        handle_tool,
    )
    .await?;
    Ok(())
}

async fn handle_tool(req: SyscallRequest) -> SyscallResponse {
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
        tokio::task::spawn_blocking(move || {
            susi_gmcp::plane_tools::execute_tool(&name, &args, &workspace)
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(format!("tool task failed: {e}")))
    };
    let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    // The tool registry flattens failures (EaiError display, capability
    // gaps, reflex errors) into text; never report those as a success.
    let outcome = outcome.and_then(|text| {
        if looks_like_tool_failure(&text) {
            Err(text)
        } else {
            Ok(text)
        }
    });
    match outcome {
        Ok(text) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Success,
            data: serde_json::json!({ "result": text }),
            receipt: None,
            latency_us,
            message: None,
        },
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us,
            message: Some(e),
        },
    }
}

/// True when tool output is a flattened failure rather than a result:
/// registry markers anywhere, or an `EaiError`-style display near the head
/// (same rule as `GemiEngine::looks_like_error_text`).
fn looks_like_tool_failure(text: &str) -> bool {
    let t = text.trim_start();
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
mod tests {
    use super::looks_like_tool_failure;

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
        assert!(!looks_like_tool_failure("SUSI Engine Version: 0.14.0"));
        assert!(!looks_like_tool_failure(
            "Wrote 2048 bytes to notes/incident-review.txt; the file quotes an old Sandbox Error: line"
        ));
    }
}
