#![forbid(unsafe_code)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{
    token_matches, SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};
use susi_abi::wire::{FrameStream, MessageType, WireFrame};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = cell_bind_addr(cell_ports::GAWD);
    println!("susi-gawd micro-daemon starting on {bind_addr}");

    let mut cell = SwarmCell::new(
        "gawd-planner-01".to_string(),
        SwarmRole::PlannerCell,
        format!("tcp://{bind_addr}"),
    );

    // Register capabilities
    cell.register_capability("swarm-scheduling");
    cell.register_capability("autonomous-planner");

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
        .unwrap_or_else(susi_gawd::susi_config::SusiConfig::ensure_api_auth_token_seeded)
        .into();

    let listener = TcpListener::bind(bind_addr).await?;
    println!("susi-gawd Swarm Cell ready and listening...");

    loop {
        let (mut socket, _) = listener.accept().await?;
        let cell_clone = Arc::clone(&cell);
        let expected_token = Arc::clone(&expected_token);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            let mut frames = FrameStream::new();
            'conn: loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
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
                                            handle_plan(req).await
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

async fn handle_plan(req: SyscallRequest) -> SyscallResponse {
    // Run the mission through the real master agent; an empty goal is an
    // error, never a fabricated "plan executed" success.
    let goal = req
        .payload
        .get("goal")
        .or_else(|| req.payload.get("intent"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let workspace = std::path::PathBuf::from(req.workspace.as_deref().unwrap_or("."));
    let started = std::time::Instant::now();
    let outcome = if goal.trim().is_empty() {
        Err("missing goal in payload (`goal`)".to_string())
    } else {
        tokio::task::spawn_blocking(move || {
            susi_gawd::ama::SusiMasterAgent::new().solve_stream_report(
                &goal,
                &workspace,
                env!("CARGO_PKG_VERSION"),
                &|_| {},
            )
        })
        .await
        .map_err(|e| format!("planner task failed: {e}"))
    };
    let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    match outcome {
        // Outcome comes from the mission report, never from the prose.
        Ok(report) => {
            let ok = report.is_success();
            SyscallResponse {
                id: req.id,
                status: if ok {
                    SyscallStatus::Success
                } else {
                    SyscallStatus::Error
                },
                data: serde_json::json!({
                    "result": report.final_answer,
                    "mission_status": report.status,
                }),
                receipt: None,
                latency_us,
                message: (!ok).then(|| format!("mission ended {}", report.status)),
            }
        }
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us,
            message: Some(e),
        },
    }
}
