#![forbid(unsafe_code)]

use std::sync::Arc;

use susi_gemi::engines::runtime::GemiEngine;
use susi_gemi::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gemi::susi_abi::swarm::SwarmRole;
use susi_gemi::susi_abi::syscall::{
    SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};

use susi_gemi::susi_abi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = cell_bind_addr(cell_ports::GEMI);
    println!("susi-gemi micro-daemon starting on {bind_addr}");

    let mut cell = SwarmCell::new(
        "gemi-infer-01".to_string(),
        SwarmRole::InferenceDriver,
        format!("tcp://{bind_addr}"),
    );

    // Register capabilities
    cell.register_capability("inference");
    cell.register_capability("text-generation");

    // Every syscall must present the host API token (or an explicit
    // SUSI_CELL_TOKEN override); heartbeats stay open for liveness.
    let expected_token: Arc<str> = match std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        Some(token) => token.into(),
        None => susi_gemi::susi_config::SusiConfig::ensure_api_auth_token_seeded()?.into(),
    };

    susi_abi::cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "susi-gemi Swarm Cell",
        handle_infer,
    )?;
    Ok(())
}

fn handle_infer(req: SyscallRequest) -> SyscallResponse {
    let prompt = req
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let started = std::time::Instant::now();
    let latency = |started: std::time::Instant| {
        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
    };
    if prompt.trim().is_empty() {
        return SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us: latency(started),
            message: Some("missing prompt in payload (`prompt`)".into()),
        };
    }
    // Inference is CPU/GPU-bound and blocking: it runs on this connection's
    // own thread (the cell server answers a panic as an `Error`), in the
    // caller's workspace.
    let ws = std::path::PathBuf::from(req.workspace.as_deref().unwrap_or("."));
    match GemiEngine::generate_reasoning(&prompt, &ws) {
        // Engines flatten failures into text; classify with the engine's own
        // marker check instead of reporting failure prose as a success.
        text if GemiEngine::looks_like_error_text(&text) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::json!({ "text": text }),
            receipt: None,
            latency_us: latency(started),
            message: Some("inference failed".into()),
        },
        text => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Success,
            data: serde_json::json!({ "text": text }),
            receipt: None,
            latency_us: latency(started),
            message: None,
        },
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use susi_gemi::susi_abi::syscall::SyscallOp;

    fn infer_request(id: &str, payload: serde_json::Value) -> SyscallRequest {
        SyscallRequest {
            id: id.to_string(),
            caller_id: "test-caller".to_string(),
            op: SyscallOp::Infer,
            token: None,
            workspace: Some(".".to_string()),
            payload,
            timestamp: 1,
        }
    }

    #[test]
    fn handle_infer_rejects_missing_prompt_field_with_error_status() {
        let resp = handle_infer(infer_request("inf-1", serde_json::json!({})));
        assert_eq!(resp.id, "inf-1");
        assert_eq!(resp.status, SyscallStatus::Error);
        assert!(resp
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("missing prompt"));
    }

    #[test]
    fn handle_infer_rejects_whitespace_only_prompt_with_error_status() {
        let resp = handle_infer(infer_request(
            "inf-2",
            serde_json::json!({ "prompt": "   \t\n " }),
        ));
        assert_eq!(resp.id, "inf-2");
        assert_eq!(resp.status, SyscallStatus::Error);
        assert_eq!(resp.data, serde_json::Value::Null);
    }

    #[test]
    fn handle_infer_rejects_non_string_prompt_payload_with_error_status() {
        let resp = handle_infer(infer_request("inf-3", serde_json::json!({ "prompt": 123 })));
        assert_eq!(resp.id, "inf-3");
        assert_eq!(resp.status, SyscallStatus::Error);
    }

    #[test]
    fn inference_swarm_cell_registers_inference_and_text_generation_capabilities() {
        let bind_addr = cell_bind_addr(cell_ports::GEMI);
        let mut cell = SwarmCell::new(
            "gemi-infer-01".to_string(),
            SwarmRole::InferenceDriver,
            format!("tcp://{bind_addr}"),
        );
        cell.register_capability("inference");
        cell.register_capability("text-generation");
        assert_eq!(cell.manifest.role, SwarmRole::InferenceDriver);
        assert_eq!(cell.manifest.capabilities.len(), 2);
    }
}
