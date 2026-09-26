//! Swarm-cell TCP server shared by every cell binary (`susi-gawd`,
//! `susi-gemi`, `susi-gmcp`, `susi-dsh-cell`, `susi-universal-cell`).
//! Canonical source, `#[path]`-mounted by each binary; it is not part of the
//! embedded ABI because it needs tokio. Mounting binaries expose their ABI
//! module as `crate::susi_abi`.
//!
//! Per connection: bytes feed a [`FrameStream`]; each `SyscallRequest` frame
//! is token-checked, dispatched to the cell's handler, scored into the
//! cell's trust record, and answered with a `SyscallResponse` frame;
//! `Heartbeat` frames are answered with the cell's health snapshot. A
//! corrupt stream drops the connection — framing cannot be recovered.

use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::susi_abi::cell::SwarmCell;
use crate::susi_abi::syscall::{token_matches, SyscallRequest, SyscallResponse, SyscallStatus};
use crate::susi_abi::wire::{FrameStream, MessageType, WireFrame};

/// Serve `cell` on `bind_addr` until the listener fails, answering every
/// authenticated syscall with `handler`. Every syscall must present
/// `expected_token`; an empty token denies all of them. Heartbeats stay
/// open for liveness.
pub(crate) async fn serve<H, Fut>(
    mut cell: SwarmCell,
    bind_addr: impl tokio::net::ToSocketAddrs,
    expected_token: Arc<str>,
    banner: &str,
    handler: H,
) -> std::io::Result<()>
where
    H: Fn(SyscallRequest) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = SyscallResponse> + Send + 'static,
{
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    cell.set_ready(now);
    let cell = Arc::new(tokio::sync::Mutex::new(cell));

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    println!("{banner} ready and listening...");

    loop {
        let (socket, _) = listener.accept().await?;
        tokio::spawn(serve_connection(
            socket,
            Arc::clone(&cell),
            Arc::clone(&expected_token),
            handler.clone(),
        ));
    }
}

async fn serve_connection<H, Fut>(
    mut socket: tokio::net::TcpStream,
    cell: Arc<tokio::sync::Mutex<SwarmCell>>,
    expected_token: Arc<str>,
    handler: H,
) where
    H: Fn(SyscallRequest) -> Fut,
    Fut: Future<Output = SyscallResponse>,
{
    let mut buf = vec![0u8; 64 * 1024];
    let mut frames = FrameStream::new();
    loop {
        let n = match socket.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        frames.push(&buf[..n]);
        loop {
            let frame = match frames.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                // Corrupt stream: framing is lost, drop the connection.
                Err(_) => return,
            };
            let reply = match frame.msg_type {
                MessageType::SyscallRequest => {
                    let Ok(req) = serde_json::from_slice::<SyscallRequest>(&frame.payload) else {
                        continue;
                    };
                    let response = if token_matches(req.token.as_deref(), &expected_token) {
                        handler(req).await
                    } else {
                        SyscallResponse {
                            id: req.id,
                            status: SyscallStatus::Denied,
                            data: serde_json::Value::Null,
                            receipt: None,
                            latency_us: 0,
                            message: Some("missing or invalid cell token".into()),
                        }
                    };
                    // Hold the cell lock only to record the outcome, never
                    // across the work itself, so heartbeats and other
                    // connections are not blocked.
                    {
                        let mut cell = cell.lock().await;
                        match response.status {
                            SyscallStatus::Success => cell.record_success(),
                            // Auth denials are the caller's fault; they must
                            // not let an unauthenticated peer drive the
                            // cell's trust score down.
                            SyscallStatus::Denied => {}
                            SyscallStatus::NotFound
                            | SyscallStatus::Timeout
                            | SyscallStatus::Error => cell.record_failure(),
                        }
                    }
                    serde_json::to_vec(&response)
                        .ok()
                        .map(|payload| WireFrame::new(MessageType::SyscallResponse, payload))
                }
                // Answer liveness probes with the cell's health snapshot.
                MessageType::Heartbeat => {
                    let status = cell.lock().await.heartbeat_status();
                    serde_json::to_vec(&status)
                        .ok()
                        .map(|payload| WireFrame::new(MessageType::Heartbeat, payload))
                }
                MessageType::SyscallResponse
                | MessageType::SwarmPheromone
                | MessageType::RawBytes
                | MessageType::TaskNegotiation
                | MessageType::EventLog => None,
            };
            if let Some(Ok(bytes)) = reply.map(|frame| frame.encode()) {
                if socket.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        }
    }
}
