#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_abi::cell::SwarmCell;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus};
use susi_abi::wire::{FrameStream, MessageType, WireFrame};
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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    let listener = TcpListener::bind("127.0.0.1:9091").await?;
    println!("susi-gemi Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            let mut frames = FrameStream::new();
            'conn: loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break, // Connection closed
                    Ok(n) => {
                        frames.push(&buf[..n]);
                        loop {
                            let frame = match frames.next_frame() {
                                Ok(Some(frame)) => frame,
                                Ok(None) => break,
                                // Corrupt stream: framing is lost, drop the connection.
                                Err(_) => break 'conn,
                            };
                            if frame.msg_type == MessageType::SyscallRequest {
                                if let Ok(req) =
                                    serde_json::from_slice::<SyscallRequest>(&frame.payload)
                                {
                                    let response = handle_infer(req).await;
                                    // Hold the cell lock only to record the outcome, never across the
                                    // work itself, so heartbeats and other connections are not blocked.
                                    {
                                        let mut cell = cell_clone.lock().await;
                                        if response.status == SyscallStatus::Success {
                                            cell.record_success();
                                        } else {
                                            cell.record_failure();
                                        }
                                    }
                                    let Ok(resp_payload) = serde_json::to_vec(&response) else {
                                        continue;
                                    };
                                    let resp_frame =
                                        WireFrame::new(MessageType::SyscallResponse, resp_payload);
                                    let encoded = resp_frame.encode();
                                    let _ = socket.write_all(&encoded).await;
                                }
                            } else if frame.msg_type == MessageType::Heartbeat {
                                // Answer liveness probes with the cell's health snapshot.
                                let status = cell_clone.lock().await.heartbeat_status();
                                if let Ok(payload) = serde_json::to_vec(&status) {
                                    let pong = WireFrame::new(MessageType::Heartbeat, payload);
                                    let _ = socket.write_all(&pong.encode()).await;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
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
