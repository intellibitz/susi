#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[rustfmt::skip]
#[path = "../../susi-abi/src/embedded.rs"]
#[allow(dead_code)] // A cell embeds the complete stable ABI, not only today's handlers.
mod susi_abi;

use std::sync::Arc;

use crate::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use crate::susi_abi::swarm::SwarmRole;
use crate::susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV};

// Shared swarm-cell server loop (canonical: crates/susi-abi/src/cell_server.rs).
#[rustfmt::skip]
#[path = "../../susi-abi/src/cell_server.rs"]
mod cell_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = cell_bind_addr(cell_ports::DSH);
    println!("susi-dsh-cell micro-daemon starting on {bind_addr}");

    let mut cell = SwarmCell::new(
        "dsh-agent-01".to_string(),
        SwarmRole::ExternalPeer,
        format!("tcp://{bind_addr}"),
    );

    // Register capabilities
    cell.register_capability("deepseek-harness");
    cell.register_capability("dsh");
    cell.register_capability("inference");

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
        "susi-dsh-cell Swarm Cell",
        handle_dsh,
    )
    .await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use crate::susi_abi::cell::SwarmCell;
    use crate::susi_abi::swarm::SwarmRole;
    use crate::susi_abi::syscall::{SyscallOp, SyscallRequest, SyscallResponse, SyscallStatus};
    use crate::susi_abi::wire::{FrameStream, MessageType, WireFrame};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn round_trip(stream: &mut tokio::net::TcpStream, frame: WireFrame) -> WireFrame {
        stream.write_all(&frame.encode().unwrap()).await.unwrap();
        let mut frames = FrameStream::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = frames.next_frame().unwrap() {
                return frame;
            }
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "cell closed the connection");
            frames.push(&buf[..n]);
        }
    }

    fn request(token: Option<&str>) -> WireFrame {
        let req = SyscallRequest {
            id: "r1".into(),
            caller_id: "test".into(),
            op: SyscallOp::Infer,
            token: token.map(str::to_string),
            workspace: None,
            payload: serde_json::json!({"prompt": "hi"}),
            timestamp: 0,
        };
        WireFrame::new(
            MessageType::SyscallRequest,
            serde_json::to_vec(&req).unwrap(),
        )
    }

    /// The shared cell server end to end: heartbeats answer unauthenticated,
    /// a wrong token is denied without reaching the handler, and a valid
    /// token dispatches to it.
    #[tokio::test]
    async fn shared_cell_server_authenticates_and_dispatches() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let cell = SwarmCell::new(
            "test-cell".into(),
            SwarmRole::ExternalPeer,
            format!("tcp://127.0.0.1:{port}"),
        );
        tokio::spawn(crate::cell_server::serve(
            cell,
            ("127.0.0.1", port),
            Arc::from("secret"),
            "test cell",
            |req: SyscallRequest| async move {
                SyscallResponse {
                    id: req.id,
                    status: SyscallStatus::Success,
                    data: req.payload,
                    receipt: None,
                    latency_us: 0,
                    message: None,
                }
            },
        ));
        let mut stream = loop {
            if let Ok(s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                break s;
            }
            tokio::task::yield_now().await;
        };

        let pong = round_trip(
            &mut stream,
            WireFrame::new(MessageType::Heartbeat, Vec::new()),
        )
        .await;
        assert_eq!(pong.msg_type, MessageType::Heartbeat);

        let denied = round_trip(&mut stream, request(Some("wrong"))).await;
        let denied: SyscallResponse = serde_json::from_slice(&denied.payload).unwrap();
        assert_eq!(denied.status, SyscallStatus::Denied);

        let ok = round_trip(&mut stream, request(Some("secret"))).await;
        assert_eq!(ok.msg_type, MessageType::SyscallResponse);
        let ok: SyscallResponse = serde_json::from_slice(&ok.payload).unwrap();
        assert_eq!(ok.status, SyscallStatus::Success);
        assert_eq!(ok.data, serde_json::json!({"prompt": "hi"}));
    }
}
