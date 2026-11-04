//! Swarm-cell TCP server shared by every cell binary (`susi-gawd`,
//! `susi-gemi`, `susi-gmcp`, `susi-universal-cell`).
//! Compiled only with the `cell-server` feature (needs tokio). Type-only
//! consumers of this crate leave that feature off.
//!
//! Per connection: bytes feed a [`FrameStream`]; each `SyscallRequest` frame
//! is token-checked, dispatched to the cell's handler, scored into the
//! cell's trust record, and answered with a `SyscallResponse` frame;
//! `Heartbeat` frames are answered with the cell's health snapshot. A
//! corrupt stream drops the connection — framing cannot be recovered.

use std::future::Future;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cell::SwarmCell;
use crate::syscall::{token_matches, SyscallRequest, SyscallResponse, SyscallStatus};
use crate::wire::{FrameStream, MessageType, WireFrame};

/// Serve `cell` on `bind_addr` until the listener fails, answering every
/// authenticated syscall with `handler`. Every syscall must present
/// `expected_token`; an empty token denies all of them. Heartbeats stay
/// open for liveness.
pub async fn serve<H, Fut>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::SwarmRole;
    use crate::syscall::SyscallOp;

    async fn read_one_frame(stream: &mut tokio::net::TcpStream) -> WireFrame {
        let mut decoder = FrameStream::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = decoder.next_frame().expect("valid wire frame") {
                return frame;
            }
            let n = stream.read(&mut buf).await.expect("read socket bytes");
            assert!(n > 0, "unexpected EOF while waiting for frame");
            decoder.push(&buf[..n]);
        }
    }

    #[tokio::test]
    async fn cell_server_denies_bad_token_without_trust_decay_and_scores_authenticated_syscalls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let mut cell = SwarmCell::new(
            "cell-loopback-01".to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{addr}"),
        );
        cell.set_ready(100);
        let shared_cell = Arc::new(tokio::sync::Mutex::new(cell));
        let token: Arc<str> = Arc::from("top-secret-cell-token");

        let server_cell = Arc::clone(&shared_cell);
        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept client");
            serve_connection(socket, server_cell, token, |req| async move {
                SyscallResponse {
                    id: req.id,
                    status: SyscallStatus::Success,
                    data: serde_json::json!({"echo_op": req.op}),
                    receipt: None,
                    latency_us: 5,
                    message: None,
                }
            })
            .await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback client");

        // 1. Unauthenticated request is Denied and must NOT penalize trust_score or missions_failed.
        let unauth_req = SyscallRequest {
            id: "req-unauth".to_string(),
            caller_id: "peer".to_string(),
            op: SyscallOp::ToolCall,
            token: Some("wrong-token".to_string()),
            workspace: None,
            payload: serde_json::Value::Null,
            timestamp: 101,
        };
        let unauth_bytes = WireFrame::new(
            MessageType::SyscallRequest,
            serde_json::to_vec(&unauth_req).expect("serialize unauth_req"),
        )
        .encode()
        .expect("encode unauth frame");
        client
            .write_all(&unauth_bytes)
            .await
            .expect("write unauth frame");

        let unauth_reply_frame = read_one_frame(&mut client).await;
        assert_eq!(unauth_reply_frame.msg_type, MessageType::SyscallResponse);
        let unauth_resp: SyscallResponse =
            serde_json::from_slice(&unauth_reply_frame.payload).expect("decode unauth response");
        assert_eq!(unauth_resp.id, "req-unauth");
        assert_eq!(unauth_resp.status, SyscallStatus::Denied);

        // 2. Authenticated request succeeds and increments missions_completed.
        let auth_req = SyscallRequest {
            id: "req-auth".to_string(),
            caller_id: "daemon".to_string(),
            op: SyscallOp::ToolCall,
            token: Some("top-secret-cell-token".to_string()),
            workspace: None,
            payload: serde_json::json!({"tool": "ping"}),
            timestamp: 102,
        };
        let auth_bytes = WireFrame::new(
            MessageType::SyscallRequest,
            serde_json::to_vec(&auth_req).expect("serialize auth_req"),
        )
        .encode()
        .expect("encode auth frame");
        client
            .write_all(&auth_bytes)
            .await
            .expect("write auth frame");

        let auth_reply_frame = read_one_frame(&mut client).await;
        assert_eq!(auth_reply_frame.msg_type, MessageType::SyscallResponse);
        let auth_resp: SyscallResponse =
            serde_json::from_slice(&auth_reply_frame.payload).expect("decode auth response");
        assert_eq!(auth_resp.id, "req-auth");
        assert_eq!(auth_resp.status, SyscallStatus::Success);
        assert_eq!(auth_resp.data["echo_op"], "tool_call");

        // 3. Heartbeat frame returns live cell counters proving trust_score == 1.0,
        // missions_failed == 0, and missions_completed == 1.
        let hb_bytes = WireFrame::new(MessageType::Heartbeat, Vec::new())
            .encode()
            .expect("encode heartbeat frame");
        client.write_all(&hb_bytes).await.expect("write heartbeat");

        let hb_reply_frame = read_one_frame(&mut client).await;
        assert_eq!(hb_reply_frame.msg_type, MessageType::Heartbeat);
        let hb_status: serde_json::Value =
            serde_json::from_slice(&hb_reply_frame.payload).expect("decode heartbeat status");
        assert_eq!(hb_status["cell_id"], "cell-loopback-01");
        assert_eq!(hb_status["missions_completed"], 1);
        assert_eq!(hb_status["missions_failed"], 0);
        assert_eq!(hb_status["trust_score"], 1.0);

        drop(client);
        server_task.await.expect("join server task");
    }

    #[tokio::test]
    async fn cell_server_empty_expected_token_denies_all_syscalls_without_trust_decay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let mut cell = SwarmCell::new(
            "cell-empty-token".to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{addr}"),
        );
        cell.set_ready(200);
        let shared_cell = Arc::new(tokio::sync::Mutex::new(cell));
        let empty_token: Arc<str> = Arc::from("");

        let server_cell = Arc::clone(&shared_cell);
        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept client");
            serve_connection(socket, server_cell, empty_token, |req| async move {
                SyscallResponse {
                    id: req.id,
                    status: SyscallStatus::Success,
                    data: serde_json::Value::Null,
                    receipt: None,
                    latency_us: 1,
                    message: None,
                }
            })
            .await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback client");

        for (id, token) in [
            ("req-empty", Some(String::new())),
            ("req-nonempty", Some("some-token".to_string())),
            ("req-none", None),
        ] {
            let req = SyscallRequest {
                id: id.to_string(),
                caller_id: "peer".to_string(),
                op: SyscallOp::Infer,
                token,
                workspace: None,
                payload: serde_json::Value::Null,
                timestamp: 201,
            };
            let bytes = WireFrame::new(
                MessageType::SyscallRequest,
                serde_json::to_vec(&req).expect("serialize req"),
            )
            .encode()
            .expect("encode req");
            client.write_all(&bytes).await.expect("write req");

            let reply = read_one_frame(&mut client).await;
            let resp: SyscallResponse =
                serde_json::from_slice(&reply.payload).expect("decode resp");
            assert_eq!(resp.id, id);
            assert_eq!(resp.status, SyscallStatus::Denied);
            assert_eq!(
                resp.message.as_deref(),
                Some("missing or invalid cell token")
            );
        }

        let hb_bytes = WireFrame::new(MessageType::Heartbeat, Vec::new())
            .encode()
            .expect("encode heartbeat");
        client.write_all(&hb_bytes).await.expect("write heartbeat");
        let hb_reply = read_one_frame(&mut client).await;
        let hb_status: serde_json::Value =
            serde_json::from_slice(&hb_reply.payload).expect("decode heartbeat");
        assert_eq!(hb_status["missions_completed"], 0);
        assert_eq!(hb_status["missions_failed"], 0);
        assert_eq!(hb_status["trust_score"], 1.0);

        drop(client);
        server_task.await.expect("join server task");
    }

    #[tokio::test]
    async fn cell_server_records_failures_for_not_found_timeout_and_error_statuses() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let mut cell = SwarmCell::new(
            "cell-failure-scoring".to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{addr}"),
        );
        cell.set_ready(300);
        let shared_cell = Arc::new(tokio::sync::Mutex::new(cell));
        let token: Arc<str> = Arc::from("score-token");

        let server_cell = Arc::clone(&shared_cell);
        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept client");
            serve_connection(socket, server_cell, token, |req| async move {
                let status = match req.id.as_str() {
                    "req-not-found" => SyscallStatus::NotFound,
                    "req-timeout" => SyscallStatus::Timeout,
                    _ => SyscallStatus::Error,
                };
                SyscallResponse {
                    id: req.id,
                    status,
                    data: serde_json::Value::Null,
                    receipt: None,
                    latency_us: 10,
                    message: Some("simulated failure".to_string()),
                }
            })
            .await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback client");

        for (id, expected_status) in [
            ("req-not-found", SyscallStatus::NotFound),
            ("req-timeout", SyscallStatus::Timeout),
            ("req-error", SyscallStatus::Error),
        ] {
            let req = SyscallRequest {
                id: id.to_string(),
                caller_id: "daemon".to_string(),
                op: SyscallOp::ToolCall,
                token: Some("score-token".to_string()),
                workspace: None,
                payload: serde_json::Value::Null,
                timestamp: 301,
            };
            let bytes = WireFrame::new(
                MessageType::SyscallRequest,
                serde_json::to_vec(&req).expect("serialize req"),
            )
            .encode()
            .expect("encode req");
            client.write_all(&bytes).await.expect("write req");

            let reply = read_one_frame(&mut client).await;
            let resp: SyscallResponse =
                serde_json::from_slice(&reply.payload).expect("decode resp");
            assert_eq!(resp.id, id);
            assert_eq!(resp.status, expected_status);
        }

        let hb_bytes = WireFrame::new(MessageType::Heartbeat, Vec::new())
            .encode()
            .expect("encode heartbeat");
        client.write_all(&hb_bytes).await.expect("write heartbeat");
        let hb_reply = read_one_frame(&mut client).await;
        let hb_status: serde_json::Value =
            serde_json::from_slice(&hb_reply.payload).expect("decode heartbeat");
        assert_eq!(hb_status["missions_completed"], 0);
        assert_eq!(hb_status["missions_failed"], 3);
        let trust = hb_status["trust_score"].as_f64().expect("trust_score f64");
        assert!(trust < 1.0, "expected decayed trust_score, got {trust}");

        drop(client);
        server_task.await.expect("join server task");
    }

    #[tokio::test]
    async fn cell_server_ignores_malformed_json_and_non_request_frames_and_drops_on_corrupt_wire() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let mut cell = SwarmCell::new(
            "cell-framing-guard".to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{addr}"),
        );
        cell.set_ready(400);
        let shared_cell = Arc::new(tokio::sync::Mutex::new(cell));
        let token: Arc<str> = Arc::from("wire-token");

        let server_cell = Arc::clone(&shared_cell);
        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept client");
            serve_connection(socket, server_cell, token, |req| async move {
                SyscallResponse {
                    id: req.id,
                    status: SyscallStatus::Success,
                    data: serde_json::json!({"ok": true}),
                    receipt: None,
                    latency_us: 2,
                    message: None,
                }
            })
            .await;
        });

        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback client");

        // 1. Send malformed JSON inside a SyscallRequest frame plus non-request/non-heartbeat frames.
        let malformed_req =
            WireFrame::new(MessageType::SyscallRequest, b"{not valid json".to_vec())
                .encode()
                .expect("encode malformed req");
        let pheromone_frame = WireFrame::new(MessageType::SwarmPheromone, b"{}".to_vec())
            .encode()
            .expect("encode pheromone");
        let raw_bytes_frame = WireFrame::new(MessageType::RawBytes, vec![1, 2, 3])
            .encode()
            .expect("encode raw bytes");
        client
            .write_all(&malformed_req)
            .await
            .expect("write malformed req");
        client
            .write_all(&pheromone_frame)
            .await
            .expect("write pheromone");
        client
            .write_all(&raw_bytes_frame)
            .await
            .expect("write raw bytes");

        // 2. Connection must remain alive and answer a subsequent Heartbeat frame.
        let hb_bytes = WireFrame::new(MessageType::Heartbeat, Vec::new())
            .encode()
            .expect("encode heartbeat");
        client.write_all(&hb_bytes).await.expect("write heartbeat");
        let hb_reply = read_one_frame(&mut client).await;
        assert_eq!(hb_reply.msg_type, MessageType::Heartbeat);

        // 3. Send corrupt wire magic bytes; server must drop the connection cleanly.
        client
            .write_all(b"BAD!_CORRUPT_HEADER_BYTES")
            .await
            .expect("write corrupt bytes");
        server_task
            .await
            .expect("server task exits cleanly on corrupt stream");
    }
}
