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
    println!("susi-gawd micro-daemon starting on 127.0.0.1:9092");

    let mut cell = SwarmCell::new(
        "gawd-planner-01".to_string(),
        SwarmRole::PlannerCell,
        "tcp://127.0.0.1:9092".to_string(),
    );

    // Register capabilities
    cell.register_capability("swarm-scheduling");
    cell.register_capability("autonomous-planner");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    let listener = TcpListener::bind("127.0.0.1:9092").await?;
    println!("susi-gawd Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok((frame, _)) = WireFrame::decode(&buf[..n]) {
                            if frame.msg_type == MessageType::SyscallRequest {
                                if let Ok(req) =
                                    serde_json::from_slice::<SyscallRequest>(&frame.payload)
                                {
                                    let response =
                                        handle_plan(req, &mut *cell_clone.lock().await).await;
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

async fn handle_plan(req: SyscallRequest, cell: &mut SwarmCell) -> SyscallResponse {
    cell.record_success();

    SyscallResponse {
        id: req.id,
        status: SyscallStatus::Success,
        data: serde_json::json!({ "result": "plan executed successfully" }),
        receipt: None,
        latency_us: 1000,
        message: None,
    }
}
