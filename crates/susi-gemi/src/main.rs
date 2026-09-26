#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_gemi::engine::GemiEngine;
use susi_gemi::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gemi::susi_abi::swarm::SwarmRole;
use susi_gemi::susi_abi::syscall::{
    token_matches, SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};
use susi_gemi::susi_abi::wire::{FrameStream, MessageType, WireFrame};

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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    cell.set_ready(now);

    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    // Every syscall must present the host API token (or an explicit
    // SUSI_CELL_TOKEN override); heartbeats stay open for liveness.
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(susi_gemi::susi_config::SusiConfig::ensure_api_auth_token_seeded)
        .into();

    let listener = TcpListener::bind(bind_addr).await?;
    println!("susi-gemi Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);
        let expected_token = Arc::clone(&expected_token);

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
                                    let response =
                                        if token_matches(req.token.as_deref(), &expected_token) {
                                            handle_infer(req).await
                                        } else {
                                            SyscallResponse {
                                                id: req.id,
                                                status: SyscallStatus::Denied,
                                                data: serde_json::Value::Null,
                                                receipt: None,
                                                latency_us: 0,
                                                message: Some(
                                                    "missing or invalid cell token".into(),
                                                ),
                                            }
                                        };
                                    // Hold the cell lock only to record the outcome, never across the
                                    // work itself, so heartbeats and other connections are not blocked.
                                    {
                                        let mut cell = cell_clone.lock().await;
                                        match response.status {
                                            SyscallStatus::Success => cell.record_success(),
                                            // Auth denials are the caller's fault; they must not let an
                                            // unauthenticated peer drive the cell's trust score down.
                                            SyscallStatus::Denied => {}
                                            SyscallStatus::NotFound
                                            | SyscallStatus::Timeout
                                            | SyscallStatus::Error => cell.record_failure(),
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
