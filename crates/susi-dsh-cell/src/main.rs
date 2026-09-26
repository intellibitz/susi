#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_abi::cell::SwarmCell;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{
    token_matches, SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};
use susi_abi::wire::{FrameStream, MessageType, WireFrame};

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

    // Every syscall must present the token from SUSI_CELL_TOKEN; without
    // it the cell denies all syscalls. Heartbeats stay open for liveness.
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV).unwrap_or_default().into();
    if expected_token.trim().is_empty() {
        eprintln!("SUSI_CELL_TOKEN is not set: every syscall will be denied");
    }

    let listener = TcpListener::bind("127.0.0.1:9093").await?;
    println!("susi-dsh-cell Swarm Cell ready and listening...");

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
                                            handle_dsh(req).await
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

async fn handle_dsh(req: SyscallRequest) -> SyscallResponse {
    let prompt = req
        .payload
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Execute the DeepSeek Harness CLI
    let started = std::time::Instant::now();
    let output = tokio::process::Command::new("dsh")
        .arg("--prompt")
        .arg(prompt)
        .output()
        .await;

    match output {
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
                    Some("dsh command failed".into())
                },
            }
        }
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::json!({ "error": e.to_string() }),
            receipt: None,
            latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            message: Some(format!("Failed to spawn dsh: {}", e)),
        },
    }
}
