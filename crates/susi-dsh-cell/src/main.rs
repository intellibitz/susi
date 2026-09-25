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
    println!("susi-dsh-cell micro-daemon starting on 127.0.0.1:9093");

    let mut cell = SwarmCell::new(
        "dsh-agent-01".to_string(),
        SwarmRole::ExternalPeer,
        "tcp://127.0.0.1:9093".to_string(),
    );

    // Register capabilities
    cell.register_capability("deepseek-harness");
    cell.register_capability("dsh");
    cell.register_capability("inference");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    let listener = TcpListener::bind("127.0.0.1:9093").await?;
    println!("susi-dsh-cell Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);

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
                                        handle_dsh(req, &mut *cell_clone.lock().await).await;
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

async fn handle_dsh(req: SyscallRequest, cell: &mut SwarmCell) -> SyscallResponse {
    let prompt = req
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Execute the DeepSeek Harness CLI
    let output = tokio::process::Command::new("dsh")
        .arg("--prompt")
        .arg(prompt)
        .output()
        .await;

    match output {
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
                message: if out.status.success() { None } else { Some("dsh command failed".into()) },
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
                message: Some(format!("Failed to spawn dsh: {}", e)),
            }
        }
    }
}
