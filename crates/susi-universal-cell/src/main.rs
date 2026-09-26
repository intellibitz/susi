#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[rustfmt::skip]
#[path = "../../susi-abi/src/embedded.rs"]
#[allow(dead_code)] // A universal cell must retain every ABI operation it can be assigned.
mod susi_abi;

use serde::Deserialize;
use std::sync::Arc;
use tokio::process::Command;

use crate::susi_abi::cell::SwarmCell;
use crate::susi_abi::swarm::SwarmRole;
use crate::susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV};

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    role: String,
    capabilities: Vec<String>,
    command: String,
    args: Vec<String>,
}

// Shared swarm-cell server loop (canonical: crates/susi-abi/src/cell_server.rs).
#[rustfmt::skip]
#[path = "../../susi-abi/src/cell_server.rs"]
mod cell_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: susi-universal-cell <bind_addr> <manifest.json>");
        std::process::exit(1);
    }

    let bind_addr = &args[1];
    let manifest_path = &args[2];

    let manifest_str = tokio::fs::read_to_string(manifest_path).await?;
    let manifest: PluginManifest = serde_json::from_str(&manifest_str)?;

    println!(
        "susi-universal-cell managing ecosystem plugin '{}' on {}",
        manifest.name, bind_addr
    );

    let role = match manifest.role.as_str() {
        "ToolDriver" => SwarmRole::ToolDriver,
        "InferenceDriver" => SwarmRole::InferenceDriver,
        "PlannerCell" => SwarmRole::PlannerCell,
        _ => SwarmRole::ExternalPeer,
    };

    let mut cell = SwarmCell::new(
        format!("universal-{}", manifest.name),
        role,
        format!("tcp://{}", bind_addr),
    );

    for cap in &manifest.capabilities {
        cell.register_capability(cap);
    }

    let manifest = Arc::new(manifest);

    // Every syscall must present the token from SUSI_CELL_TOKEN; without
    // it the cell denies all syscalls. Heartbeats stay open for liveness.
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV).unwrap_or_default().into();
    if expected_token.trim().is_empty() {
        eprintln!("SUSI_CELL_TOKEN is not set: every syscall will be denied");
    }

    cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "Universal Ecosystem Cell",
        move |req| {
            let manifest = Arc::clone(&manifest);
            async move { handle_external(req, &manifest).await }
        },
    )
    .await?;
    Ok(())
}

async fn handle_external(req: SyscallRequest, manifest: &PluginManifest) -> SyscallResponse {
    // For MCP stdio protocols or CLI agents, we proxy the JSON request via stdin.
    // As a generic adapter, we just shell out with the payload.
    let payload_str = serde_json::to_string(&req.payload).unwrap_or_default();

    let mut cmd = Command::new(&manifest.command);
    for arg in &manifest.args {
        cmd.arg(arg);
    }
    cmd.arg(&payload_str);

    let started = std::time::Instant::now();
    match cmd.output().await {
        Ok(out) => {
            let result_text = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr_text = String::from_utf8_lossy(&out.stderr).to_string();

            SyscallResponse {
                id: req.id,
                status: if out.status.success() {
                    SyscallStatus::Success
                } else {
                    SyscallStatus::Error
                },
                data: serde_json::json!({ "stdout": result_text, "stderr": stderr_text, "code": out.status.code() }),
                receipt: None,
                latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                message: if out.status.success() {
                    None
                } else {
                    Some("Ecosystem execution failed".into())
                },
            }
        }
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::json!({ "error": e.to_string() }),
            receipt: None,
            latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            message: Some(format!("Failed to spawn ecosystem plugin: {}", e)),
        },
    }
}
