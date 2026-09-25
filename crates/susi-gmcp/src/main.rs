#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_abi::cell::SwarmCell;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus};
use susi_abi::wire::{MessageType, WireFrame};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("susi-gmcp micro-daemon starting on 127.0.0.1:9090");

    let mut cell = SwarmCell::new(
        "gmcp-tools-01".to_string(),
        SwarmRole::ToolDriver,
        "tcp://127.0.0.1:9090".to_string(),
    );

    // Register capabilities
    cell.register_capability("mcp-protocol");
    cell.register_capability("system-tools");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    let listener = TcpListener::bind("127.0.0.1:9090").await?;
    println!("susi-gmcp Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break, // Connection closed
                    Ok(n) => {
                        if let Ok((frame, _)) = WireFrame::decode(&buf[..n]) {
                            if frame.msg_type == MessageType::SyscallRequest {
                                if let Ok(req) =
                                    serde_json::from_slice::<SyscallRequest>(&frame.payload)
                                {
                                    let response =
                                        handle_tool(req, &mut *cell_clone.lock().await).await;
                                    let Ok(resp_payload) = serde_json::to_vec(&response) else {
                                        continue;
                                    };
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

async fn handle_tool(req: SyscallRequest, cell: &mut SwarmCell) -> SyscallResponse {
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
    match outcome {
        Ok(text) => {
            cell.record_success();
            SyscallResponse {
                id: req.id,
                status: SyscallStatus::Success,
                data: serde_json::json!({ "result": text }),
                receipt: None,
                latency_us,
                message: None,
            }
        }
        Err(e) => {
            cell.record_failure();
            SyscallResponse {
                id: req.id,
                status: SyscallStatus::Error,
                data: serde_json::Value::Null,
                receipt: None,
                latency_us,
                message: Some(e),
            }
        }
    }
}
