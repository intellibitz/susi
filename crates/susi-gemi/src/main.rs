#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_abi::cell::SwarmCell;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus};
use susi_abi::wire::{MessageType, WireFrame};
use susi_gemi::engine::GemiEngine;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("susi-gemi micro-daemon starting on 127.0.0.1:9091");

    let mut cell = SwarmCell::new(
        "gemi-infer-01".to_string(),
        SwarmRole::InferenceDriver,
        "tcp://127.0.0.1:9091".to_string(),
    );
    
    // Register capabilities
    cell.register_capability("inference");
    cell.register_capability("text-generation");
    
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    cell.set_ready(now);
    
    let cell = Arc::new(tokio::sync::Mutex::new(cell));
    
    let listener = TcpListener::bind("127.0.0.1:9091").await?;
    println!("susi-gemi Swarm Cell ready and listening...");
    
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
                                if let Ok(req) = serde_json::from_slice::<SyscallRequest>(&frame.payload) {
                                    let response = handle_infer(req, &mut *cell_clone.lock().await).await;
                                    let resp_payload = serde_json::to_vec(&response).unwrap();
                                    let resp_frame = WireFrame::new(MessageType::SyscallResponse, resp_payload);
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

async fn handle_infer(req: SyscallRequest, cell: &mut SwarmCell) -> SyscallResponse {
    // We are simulating the inference call through GemiEngine
    // For a real implementation, we extract the prompt from req.payload
    let prompt = req.payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    
    let ws = std::path::PathBuf::from(".");
    let result = GemiEngine::generate_reasoning(prompt, &ws);
    
    cell.record_success();
    
    SyscallResponse {
        id: req.id,
        status: SyscallStatus::Success,
        data: serde_json::json!({ "text": result }),
        receipt: None,
        latency_us: 5000,
        message: None,
    }
}
