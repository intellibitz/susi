#![forbid(unsafe_code)]

use std::sync::Arc;

use susi_gemi::engine::GemiEngine;
use susi_gemi::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gemi::susi_abi::swarm::SwarmRole;
use susi_gemi::susi_abi::syscall::{
    SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};

use susi_gemi::susi_abi;

// Shared swarm-cell server loop (canonical: crates/susi-abi/src/cell_server.rs).
#[rustfmt::skip]
#[path = "../../susi-abi/src/cell_server.rs"]
mod cell_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(susi_gemi::susi_config::SusiConfig::ensure_api_auth_token_seeded)
        .into();

    cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "susi-gemi Swarm Cell",
        handle_infer,
    )
    .await?;
    Ok(())
}

async fn handle_infer(req: SyscallRequest) -> SyscallResponse {
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
    // Inference is CPU/GPU-bound and blocking: keep it off the async
    // executor, and run it in the caller's workspace.
    let ws = std::path::PathBuf::from(req.workspace.as_deref().unwrap_or("."));
    match tokio::task::spawn_blocking(move || GemiEngine::generate_reasoning(&prompt, &ws)).await {
        // Engines flatten failures into text; classify with the engine's own
        // marker check instead of reporting failure prose as a success.
        Ok(text) if GemiEngine::looks_like_error_text(&text) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::json!({ "text": text }),
            receipt: None,
            latency_us: latency(started),
            message: Some("inference failed".into()),
        },
        Ok(text) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Success,
            data: serde_json::json!({ "text": text }),
            receipt: None,
            latency_us: latency(started),
            message: None,
        },
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us: latency(started),
            message: Some(format!("inference task failed: {e}")),
        },
    }
}
