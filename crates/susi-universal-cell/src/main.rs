#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;
use serde::Deserialize;

use susi_abi::cell::SwarmCell;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus};
use susi_abi::wire::{MessageType, WireFrame};

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    role: String,
    capabilities: Vec<String>,
    command: String,
    args: Vec<String>,
}

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

    println!("susi-universal-cell managing ecosystem plugin '{}' on {}", manifest.name, bind_addr);

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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));
    let manifest = Arc::new(manifest);

    let listener = TcpListener::bind(bind_addr).await?;
    println!("Universal Ecosystem Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);
        let manifest_clone = Arc::clone(&manifest);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break, // Connection closed
                    Ok(n) => {
                        if let Ok((frame, _consumed)) = WireFrame::decode(&buf[..n]) {
                            if frame.msg_type == MessageType::SyscallRequest {
                                if let Ok(req) =
                                    serde_json::from_slice::<SyscallRequest>(&frame.payload)
                                {
                                    let response =
                                        handle_external(req, &mut *cell_clone.lock().await, &manifest_clone).await;
                                    #[allow(clippy::unwrap_used)]
                                    // SAFETY: serializing a known struct
                                    let resp_payload = serde_json::to_vec(&response).unwrap();
                                    let resp_frame =
                                        WireFrame::new(MessageType::SyscallResponse, resp_payload);
                                    let encoded = resp_frame.encode();
                                    let _ = socket.write_all(&encoded).await;
                                }
                            } else if frame.msg_type == MessageType::Heartbeat {
                                // Provide heartbeat response
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
}

async fn handle_external(req: SyscallRequest, cell: &mut SwarmCell, manifest: &PluginManifest) -> SyscallResponse {
    // For MCP stdio protocols or CLI agents, we proxy the JSON request via stdin.
    // As a generic adapter, we just shell out with the payload.
    let payload_str = serde_json::to_string(&req.payload).unwrap_or_default();
    
    let mut cmd = Command::new(&manifest.command);
    for arg in &manifest.args {
        cmd.arg(arg);
    }
    cmd.arg(&payload_str);
    
    match cmd.output().await {
        Ok(out) => {
            cell.record_success();
            let result_text = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr_text = String::from_utf8_lossy(&out.stderr).to_string();
            
            SyscallResponse {
                id: req.id,
                status: if out.status.success() { SyscallStatus::Success } else { SyscallStatus::Error },
                data: serde_json::json!({ "stdout": result_text, "stderr": stderr_text, "code": out.status.code() }),
                receipt: None,
                latency_us: 10000,
                message: if out.status.success() { None } else { Some("Ecosystem execution failed".into()) },
            }
        }
        Err(e) => {
            cell.record_failure();
            SyscallResponse {
                id: req.id,
                status: SyscallStatus::Error,
                data: serde_json::json!({ "error": e.to_string() }),
                receipt: None,
                latency_us: 1000,
                message: Some(format!("Failed to spawn ecosystem plugin: {}", e)),
            }
        }
    }
}
