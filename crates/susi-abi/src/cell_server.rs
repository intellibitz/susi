//! Swarm-cell TCP server shared by every cell binary (`susi-gawd`,
//! `susi-gemi`, `susi-gmcp`).
//!
//! Standard library only: one listener, one thread per connection, no async
//! runtime. The ABI crate therefore compiles without tokio, and the host
//! build's `susi-core`, which depends on this crate for its wire types, never
//! waits for tokio.
//!
//! Per connection: bytes feed a [`FrameStream`]; each `SyscallRequest` frame
//! is token-checked, dispatched to the cell's handler, scored into the
//! cell's trust record, and answered with a `SyscallResponse` frame;
//! `Heartbeat` frames are answered with the cell's health snapshot. A
//! corrupt stream drops the connection — framing cannot be recovered. A
//! handler that panics answers its request with an `Error` status and the
//! connection stays up, as the blocking-task wrapper each cell binary used
//! before did.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::cell::SwarmCell;
use crate::syscall::{token_matches, SyscallRequest, SyscallResponse, SyscallStatus};
use crate::wire::{FrameStream, MessageType, WireFrame};

/// Serve `cell` on `bind_addr` until the listener fails, answering every
/// authenticated syscall with `handler`. Every syscall must present
/// `expected_token`; an empty token denies all of them. Heartbeats stay
/// open for liveness. Blocks the calling thread.
pub fn serve<H>(
    mut cell: SwarmCell,
    bind_addr: impl ToSocketAddrs,
    expected_token: Arc<str>,
    banner: &str,
    handler: H,
) -> std::io::Result<()>
where
    H: Fn(SyscallRequest) -> SyscallResponse + Send + Sync + 'static,
{
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    cell.set_ready(now);
    let listener = TcpListener::bind(bind_addr)?;
    println!("{banner} ready and listening...");
    serve_listener(
        &listener,
        Arc::new(Mutex::new(cell)),
        expected_token,
        handler,
    )
}

/// Accept loop over a bound `listener`. Every connection gets its own
/// thread, so a long syscall never blocks heartbeats or other callers.
fn serve_listener<H>(
    listener: &TcpListener,
    cell: Arc<Mutex<SwarmCell>>,
    expected_token: Arc<str>,
    handler: H,
) -> std::io::Result<()>
where
    H: Fn(SyscallRequest) -> SyscallResponse + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    loop {
        let (socket, _) = listener.accept()?;
        let cell = Arc::clone(&cell);
        let token = Arc::clone(&expected_token);
        let handler = Arc::clone(&handler);
        let spawned = std::thread::Builder::new()
            .name("susi-cell-conn".into())
            .spawn(move || serve_connection(socket, &cell, &token, &*handler));
        // No thread for this connection: its socket closes with the failed
        // closure and the cell keeps serving everyone else.
        if let Err(e) = spawned {
            eprintln!("cell connection dropped: cannot start its thread: {e}");
        }
    }
}

fn serve_connection<H>(
    mut socket: TcpStream,
    cell: &Mutex<SwarmCell>,
    expected_token: &str,
    handler: &H,
) where
    H: Fn(SyscallRequest) -> SyscallResponse,
{
    let mut buf = vec![0u8; 64 * 1024];
    let mut frames = FrameStream::new();
    loop {
        let n = match socket.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
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
                    let response = if token_matches(req.token.as_deref(), expected_token) {
                        dispatch(handler, req)
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
                        let mut cell = cell.lock().unwrap_or_else(|e| e.into_inner());
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
                    let status = cell
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .heartbeat_status();
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
                if socket.write_all(&bytes).is_err() {
                    return;
                }
            }
        }
    }
}

/// Run `handler` on an authenticated request. A panic inside it becomes an
/// `Error` response that names the panic, never a dropped connection and
/// never a success.
fn dispatch<H>(handler: &H, req: SyscallRequest) -> SyscallResponse
where
    H: Fn(SyscallRequest) -> SyscallResponse,
{
    let id = req.id.clone();
    let started = std::time::Instant::now();
    catch_unwind(AssertUnwindSafe(|| handler(req))).unwrap_or_else(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-text panic payload".to_string());
        SyscallResponse {
            id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            message: Some(format!("cell handler panicked: {detail}")),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::SwarmRole;
    use crate::syscall::SyscallOp;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Every socket read and every wait in these tests is bounded: a broken
    /// server fails the test instead of hanging it.
    const WAIT: Duration = Duration::from_secs(10);

    fn ready_cell(id: &str, addr: std::net::SocketAddr, now: u64) -> Arc<Mutex<SwarmCell>> {
        let mut cell = SwarmCell::new(
            id.to_string(),
            SwarmRole::ToolDriver,
            format!("tcp://{addr}"),
        );
        cell.set_ready(now);
        Arc::new(Mutex::new(cell))
    }

    /// Serve exactly one accepted connection on a background thread; the
    /// returned receiver fires when `serve_connection` returns.
    fn serve_one<H>(
        listener: TcpListener,
        cell: Arc<Mutex<SwarmCell>>,
        token: &str,
        handler: H,
    ) -> mpsc::Receiver<()>
    where
        H: Fn(SyscallRequest) -> SyscallResponse + Send + 'static,
    {
        let token: Arc<str> = Arc::from(token);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (socket, _) = listener.accept().expect("accept client");
            serve_connection(socket, &cell, &token, &handler);
            let _ = done_tx.send(());
        });
        done_rx
    }

    fn connect(addr: std::net::SocketAddr) -> TcpStream {
        let client = TcpStream::connect_timeout(&addr, WAIT).expect("connect loopback client");
        client
            .set_read_timeout(Some(WAIT))
            .expect("bound client reads");
        client
    }

    fn read_one_frame(stream: &mut TcpStream) -> WireFrame {
        let mut decoder = FrameStream::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = decoder.next_frame().expect("valid wire frame") {
                return frame;
            }
            let n = stream.read(&mut buf).expect("read socket bytes in time");
            assert!(n > 0, "unexpected EOF while waiting for frame");
            decoder.push(&buf[..n]);
        }
    }

    fn send_request(client: &mut TcpStream, req: &SyscallRequest) -> SyscallResponse {
        let bytes = WireFrame::new(
            MessageType::SyscallRequest,
            serde_json::to_vec(req).expect("serialize request"),
        )
        .encode()
        .expect("encode request frame");
        client.write_all(&bytes).expect("write request frame");
        let reply = read_one_frame(client);
        assert_eq!(reply.msg_type, MessageType::SyscallResponse);
        serde_json::from_slice(&reply.payload).expect("decode response")
    }

    fn heartbeat(client: &mut TcpStream) -> serde_json::Value {
        let hb_bytes = WireFrame::new(MessageType::Heartbeat, Vec::new())
            .encode()
            .expect("encode heartbeat frame");
        client.write_all(&hb_bytes).expect("write heartbeat");
        let reply = read_one_frame(client);
        assert_eq!(reply.msg_type, MessageType::Heartbeat);
        serde_json::from_slice(&reply.payload).expect("decode heartbeat status")
    }

    fn request(
        id: &str,
        op: SyscallOp,
        token: Option<&str>,
        payload: serde_json::Value,
    ) -> SyscallRequest {
        SyscallRequest {
            id: id.to_string(),
            caller_id: "peer".to_string(),
            op,
            token: token.map(str::to_string),
            workspace: None,
            payload,
            timestamp: 101,
        }
    }

    fn success(req: SyscallRequest, data: serde_json::Value) -> SyscallResponse {
        SyscallResponse {
            id: req.id,
            status: SyscallStatus::Success,
            data,
            receipt: None,
            latency_us: 5,
            message: None,
        }
    }

    #[test]
    fn cell_server_denies_bad_token_without_trust_decay_and_scores_authenticated_syscalls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-loopback-01", addr, 100);
        let done = serve_one(listener, cell, "top-secret-cell-token", |req| {
            let data = serde_json::json!({"echo_op": req.op});
            success(req, data)
        });
        let mut client = connect(addr);

        // 1. Unauthenticated request is Denied and must NOT penalize trust_score or missions_failed.
        let unauth = send_request(
            &mut client,
            &request(
                "req-unauth",
                SyscallOp::ToolCall,
                Some("wrong-token"),
                serde_json::Value::Null,
            ),
        );
        assert_eq!(unauth.id, "req-unauth");
        assert_eq!(unauth.status, SyscallStatus::Denied);

        // 2. Authenticated request succeeds and increments missions_completed.
        let auth = send_request(
            &mut client,
            &request(
                "req-auth",
                SyscallOp::ToolCall,
                Some("top-secret-cell-token"),
                serde_json::json!({"tool": "ping"}),
            ),
        );
        assert_eq!(auth.id, "req-auth");
        assert_eq!(auth.status, SyscallStatus::Success);
        assert_eq!(auth.data["echo_op"], "tool_call");

        // 3. Heartbeat frame returns live cell counters proving trust_score == 1.0,
        // missions_failed == 0, and missions_completed == 1.
        let hb_status = heartbeat(&mut client);
        assert_eq!(hb_status["cell_id"], "cell-loopback-01");
        assert_eq!(hb_status["missions_completed"], 1);
        assert_eq!(hb_status["missions_failed"], 0);
        assert_eq!(hb_status["trust_score"], 1.0);

        drop(client);
        done.recv_timeout(WAIT)
            .expect("server connection ends when the client leaves");
    }

    #[test]
    fn cell_server_empty_expected_token_denies_all_syscalls_without_trust_decay() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-empty-token", addr, 200);
        let done = serve_one(listener, cell, "", |req| {
            success(req, serde_json::Value::Null)
        });
        let mut client = connect(addr);

        for (id, token) in [
            ("req-empty", Some("")),
            ("req-nonempty", Some("some-token")),
            ("req-none", None),
        ] {
            let resp = send_request(
                &mut client,
                &request(id, SyscallOp::Infer, token, serde_json::Value::Null),
            );
            assert_eq!(resp.id, id);
            assert_eq!(resp.status, SyscallStatus::Denied);
            assert_eq!(
                resp.message.as_deref(),
                Some("missing or invalid cell token")
            );
        }

        let hb_status = heartbeat(&mut client);
        assert_eq!(hb_status["missions_completed"], 0);
        assert_eq!(hb_status["missions_failed"], 0);
        assert_eq!(hb_status["trust_score"], 1.0);

        drop(client);
        done.recv_timeout(WAIT)
            .expect("server connection ends when the client leaves");
    }

    #[test]
    fn cell_server_records_failures_for_not_found_timeout_and_error_statuses() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-failure-scoring", addr, 300);
        let done = serve_one(listener, cell, "score-token", |req| {
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
        });
        let mut client = connect(addr);

        for (id, expected_status) in [
            ("req-not-found", SyscallStatus::NotFound),
            ("req-timeout", SyscallStatus::Timeout),
            ("req-error", SyscallStatus::Error),
        ] {
            let resp = send_request(
                &mut client,
                &request(
                    id,
                    SyscallOp::ToolCall,
                    Some("score-token"),
                    serde_json::Value::Null,
                ),
            );
            assert_eq!(resp.id, id);
            assert_eq!(resp.status, expected_status);
        }

        let hb_status = heartbeat(&mut client);
        assert_eq!(hb_status["missions_completed"], 0);
        assert_eq!(hb_status["missions_failed"], 3);
        let trust = hb_status["trust_score"].as_f64().expect("trust_score f64");
        assert!(trust < 1.0, "expected decayed trust_score, got {trust}");

        drop(client);
        done.recv_timeout(WAIT)
            .expect("server connection ends when the client leaves");
    }

    #[test]
    fn cell_server_ignores_malformed_json_and_non_request_frames_and_drops_on_corrupt_wire() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-framing-guard", addr, 400);
        let done = serve_one(listener, cell, "wire-token", |req| {
            success(req, serde_json::json!({"ok": true}))
        });
        let mut client = connect(addr);

        // 1. Send malformed JSON inside a SyscallRequest frame plus non-request/non-heartbeat frames.
        for frame in [
            WireFrame::new(MessageType::SyscallRequest, b"{not valid json".to_vec()),
            WireFrame::new(MessageType::SwarmPheromone, b"{}".to_vec()),
            WireFrame::new(MessageType::RawBytes, vec![1, 2, 3]),
        ] {
            let bytes = frame.encode().expect("encode frame");
            client.write_all(&bytes).expect("write frame");
        }

        // 2. Connection must remain alive and answer a subsequent Heartbeat frame.
        let hb_status = heartbeat(&mut client);
        assert_eq!(hb_status["cell_id"], "cell-framing-guard");

        // 3. Send corrupt wire magic bytes; server must drop the connection cleanly
        // while the client is still connected.
        client
            .write_all(b"BAD!_CORRUPT_HEADER_BYTES")
            .expect("write corrupt bytes");
        done.recv_timeout(WAIT)
            .expect("server drops the connection on a corrupt stream");
        // The server closed its end: the client reads EOF (or a reset on
        // hosts that abort the socket), never a reply.
        let mut rest = Vec::new();
        match client.read_to_end(&mut rest) {
            Ok(_) => assert!(rest.is_empty(), "no reply follows a corrupt stream"),
            Err(e) => assert!(
                matches!(
                    e.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                ),
                "closed connection reads EOF or a reset, got {e}"
            ),
        }
    }

    #[test]
    fn cell_server_answers_a_panicking_handler_with_an_error_and_keeps_the_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-panic-guard", addr, 500);
        let done = serve_one(listener, cell, "panic-token", |req| {
            if req.id == "req-panic" {
                panic!("planner exploded");
            }
            success(req, serde_json::Value::Null)
        });
        let mut client = connect(addr);

        let panicked = send_request(
            &mut client,
            &request(
                "req-panic",
                SyscallOp::Infer,
                Some("panic-token"),
                serde_json::Value::Null,
            ),
        );
        assert_eq!(panicked.id, "req-panic");
        assert_eq!(panicked.status, SyscallStatus::Error);
        assert_eq!(
            panicked.message.as_deref(),
            Some("cell handler panicked: planner exploded")
        );

        // Same connection, next request: the cell is still serving.
        let ok = send_request(
            &mut client,
            &request(
                "req-after",
                SyscallOp::Infer,
                Some("panic-token"),
                serde_json::Value::Null,
            ),
        );
        assert_eq!(ok.status, SyscallStatus::Success);

        let hb_status = heartbeat(&mut client);
        assert_eq!(hb_status["missions_completed"], 1);
        assert_eq!(hb_status["missions_failed"], 1);

        drop(client);
        done.recv_timeout(WAIT)
            .expect("server connection ends when the client leaves");
    }

    #[test]
    fn cell_server_answers_heartbeats_on_one_connection_while_another_syscall_runs() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let cell = ready_cell("cell-concurrent", addr, 600);
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let started_tx = Mutex::new(started_tx);
        let release_rx = Mutex::new(release_rx);
        let token: Arc<str> = Arc::from("busy-token");
        // The accept loop runs for the rest of the test binary; it owns only
        // its listener and this test's cell.
        std::thread::spawn(move || {
            let _ = serve_listener(&listener, cell, token, move |req| {
                let _ = started_tx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send(());
                let released = release_rx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .recv_timeout(WAIT);
                assert!(released.is_ok(), "slow syscall released in time");
                success(req, serde_json::Value::Null)
            });
        });

        // Connection A starts a syscall that blocks inside the handler.
        let mut busy = connect(addr);
        let busy_req = request(
            "req-slow",
            SyscallOp::ToolCall,
            Some("busy-token"),
            serde_json::Value::Null,
        );
        let busy_bytes = WireFrame::new(
            MessageType::SyscallRequest,
            serde_json::to_vec(&busy_req).expect("serialize slow request"),
        )
        .encode()
        .expect("encode slow request");
        busy.write_all(&busy_bytes).expect("write slow request");
        started_rx
            .recv_timeout(WAIT)
            .expect("slow syscall reached the handler");

        // Connection B is answered while A's handler still runs.
        let mut probe = connect(addr);
        let hb_status = heartbeat(&mut probe);
        assert_eq!(hb_status["cell_id"], "cell-concurrent");
        assert_eq!(hb_status["missions_completed"], 0);

        release_tx.send(()).expect("release slow syscall");
        let reply = read_one_frame(&mut busy);
        let resp: SyscallResponse =
            serde_json::from_slice(&reply.payload).expect("decode slow response");
        assert_eq!(resp.id, "req-slow");
        assert_eq!(resp.status, SyscallStatus::Success);
        assert_eq!(heartbeat(&mut probe)["missions_completed"], 1);
    }

    #[test]
    fn wire_frame_heartbeat_and_syscall_response_encode_decode_roundtrip() {
        let hb = WireFrame::new(MessageType::Heartbeat, b"{}".to_vec())
            .encode()
            .expect("encode heartbeat");
        let (decoded, consumed) = WireFrame::decode(&hb).expect("decode heartbeat");
        assert_eq!(consumed, hb.len());
        assert_eq!(decoded.msg_type, MessageType::Heartbeat);
        assert_eq!(decoded.payload, b"{}");
        let stream = super::super::wire::FrameStream::new();
        assert_eq!(stream.buffered(), 0);
    }
}
