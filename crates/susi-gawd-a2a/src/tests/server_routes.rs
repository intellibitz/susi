use crate::acp::{
    AcpAdmissionPolicy, AcpContentBlock, AcpEngine, AcpTurnContext, AcpTurnExecutor,
    ACP_DISCOVERY_SCHEMA, ACP_PROTOCOL_VERSION, ACP_RPC_PATH, ACP_SESSION_ID_HEADER,
    ACP_STREAM_PATH, JSONRPC_INVALID_PARAMS, WELL_KNOWN_ACP_PATH,
};
use crate::protocol::{
    AgentExecutor, EventQueue, Message, Part, PartContent, RequestContext, Result, Task, TaskState,
    TaskStatus,
};
use crate::susi_core::a2a_wire;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Replies with the received text, uppercased, as a completed task.
struct Shout;

impl AgentExecutor for Shout {
    fn execute<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let text = ctx
                .message
                .as_ref()
                .and_then(|m| m.parts.first())
                .and_then(|p| match &p.content {
                    PartContent::Text(t) => Some(t.to_uppercase()),
                    _ => None,
                })
                .unwrap_or_default();
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::with_message(
                TaskState::Completed,
                Message::agent(vec![Part::text(text)]),
            );
            queue.send(task)?;
            Ok(())
        })
    }

    fn cancel<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        _queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

struct PausingAcp {
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

impl AcpTurnExecutor for PausingAcp {
    fn execute_turn(
        &self,
        _ctx: &AcpTurnContext,
        _cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> std::result::Result<Vec<AcpContentBlock>, String> {
        emit_delta("early");
        self.entered.store(true, Ordering::Release);
        while !self.release.load(Ordering::Acquire) {
            thread::yield_now();
        }
        Ok(vec![AcpContentBlock::text("done")])
    }
}

/// The production stack around `Shout`, admitting every caller, on a
/// loopback port.
fn serve_shout(runtime: &tokio::runtime::Runtime) -> SocketAddr {
    let card = crate::executor::GawdA2AExecutor::new().agent_card();
    let app = super::app(
        Arc::new(Shout),
        Arc::new(
            crate::executor::GawdAcpExecutor::with_runner(Arc::new(|prompt: &str| {
                Ok(format!("gawd mission: {prompt}"))
            }))
            .with_workspace(crate::test_board_fixture::init_board_workspace(
                "srv-routes-shout",
            )),
        ),
        card,
        Arc::new(crate::task_store::TaskStore::new()),
        Arc::new(|_: &super::VerifierContext| true),
    );
    super::serve_on_loopback(runtime, app)
}

fn post(addr: SocketAddr, path: &str, body: &serde_json::Value) -> serde_json::Value {
    let payload = serde_json::to_vec(body).unwrap();
    let reply = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{path}"),
        &[
            ("Content-Type", "application/json"),
            ("A2A-Version", a2a_wire::A2A_VERSION),
        ],
        Some(&payload),
        20,
        0,
    )
    .unwrap();
    serde_json::from_str(&reply.into_utf8(1_000_000).unwrap()).unwrap()
}

#[test]
fn unauthorized_carries_bearer_challenge() {
    let response = super::unauthorized();
    assert_eq!(response.status(), super::StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(super::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer realm=\"susi-a2a\"")
    );
}

/// The shared outbound `message/send` shape round-trips through the real
/// JSON-RPC router: the v1.0 part is decoded as text and the task
/// result folds back into its reply.
#[test]
fn shared_message_send_round_trips_through_the_a2a_router() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve_shout(&runtime);
    let reply = post(addr, "/", &a2a_wire::message_send_request("ping"));
    assert_eq!(
        a2a_wire::reply_summary(&reply).unwrap(),
        "task completed: PING",
        "{reply}"
    );
}

/// The SSE route answers JSON-RPC errors with the standard codes too:
/// an unknown method on `/stream` is -32601 and a resubscribe to a task
/// that does not exist is A2A task-not-found (-32001), not a wrapped
/// internal error.
#[test]
fn stream_route_errors_carry_standard_codes() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve_shout(&runtime);
    let call = |body: serde_json::Value| post(addr, "/stream", &body);
    let unknown =
        call(serde_json::json!({"jsonrpc":"2.0","id":5,"method":"x/unknown","params":{}}));
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    assert_eq!(unknown["id"], 5, "{unknown}");
    let missing = call(serde_json::json!({"jsonrpc":"2.0","id":6,
        "method":"tasks/resubscribe","params":{"id":"no-such-task"}}));
    assert_eq!(missing["error"]["code"], -32001, "{missing}");
    // The A2A 1.0 name reaches the same handler.
    let subscribe = call(serde_json::json!({"jsonrpc":"2.0","id":7,
        "method":"SubscribeToTask","params":{"id":"no-such-task"}}));
    assert_eq!(subscribe["error"]["code"], -32001, "{subscribe}");
    assert_eq!(subscribe["id"], 7, "{subscribe}");
}

/// The card claims A2A 1.0, so the 1.0 method names (`SendMessage`, ...)
/// dispatch like the slash names, which stay accepted as aliases.
#[test]
fn a2a_1_0_method_names_dispatch() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve_shout(&runtime);
    let send = |method: &str| {
        let mut body = a2a_wire::message_send_request("ping");
        body["method"] = method.into();
        post(addr, "/", &body)
    };
    for method in ["SendMessage", "message/send"] {
        let reply = send(method);
        assert_eq!(
            a2a_wire::reply_summary(&reply).unwrap(),
            "task completed: PING",
            "{method}: {reply}"
        );
    }
    let get = send("GetTask");
    assert_ne!(get["error"]["code"], -32601, "GetTask must dispatch: {get}");
}

/// Envelope and params failures carry the standard JSON-RPC codes and
/// the request id; a handler refusal on `/` stays -32603.
#[test]
fn rpc_errors_carry_standard_codes_and_ids() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve_shout(&runtime);
    let unknown = post(
        addr,
        "/",
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"x","params":{}}),
    );
    assert_eq!(
        unknown["error"],
        serde_json::json!({"code":-32601,"message":"Method 'x' not found"})
    );
    assert_eq!(unknown["id"], 1);
    let params = post(
        addr,
        "/",
        &serde_json::json!({"jsonrpc":"2.0","id":9,"method":"message/send","params":{}}),
    );
    assert_eq!(params["error"]["code"], -32602, "{params}");
    assert_eq!(
        params["error"]["message"],
        "Invalid params: missing field `message`"
    );
    assert_eq!(params["id"], 9);
    let refused = post(
        addr,
        "/",
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{"id":"t"}}),
    );
    assert_eq!(
        refused,
        serde_json::json!({"jsonrpc":"2.0","id":null,
            "error":{"code":-32603,"message":"task not found: t"}})
    );
}

#[test]
fn acp_pillar_routes_mount_behind_same_guard_with_public_discovery() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let card = crate::executor::GawdA2AExecutor::new().agent_card();
    let guarded_app = super::app(
        Arc::new(Shout),
        Arc::new(
            crate::executor::GawdAcpExecutor::with_runner(Arc::new(|prompt: &str| {
                Ok(format!("gawd mission: {prompt}"))
            }))
            .with_workspace(crate::test_board_fixture::init_board_workspace(
                "srv-routes-guarded",
            )),
        ),
        card,
        Arc::new(crate::task_store::TaskStore::new()),
        Arc::new(|ctx: &super::VerifierContext| {
            ctx.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                == Some("Bearer acp-port-9094")
        }),
    );
    let addr = super::serve_on_loopback(&runtime, guarded_app);

    // Public discovery at /.well-known/acp.json needs no token.
    let disc_resp = susi_http_transport::http_call_with_body(
        "GET",
        &format!("http://{addr}{WELL_KNOWN_ACP_PATH}"),
        &[],
        None,
        20,
        0,
    )
    .unwrap();
    assert_eq!(disc_resp.status, 200);
    let disc: serde_json::Value =
        serde_json::from_str(&disc_resp.into_utf8(1_000_000).unwrap()).unwrap();
    assert_eq!(disc["schema"], ACP_DISCOVERY_SCHEMA);
    assert_eq!(disc["protocol_version"], ACP_PROTOCOL_VERSION);

    // POST /acp without token returns 401.
    let unauth = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_RPC_PATH}"),
        &[("Content-Type", "application/json")],
        Some(br#"{"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}"#),
        20,
        0,
    )
    .unwrap();
    assert_eq!(unauth.status, 401);

    // POST /acp with valid Bearer token creates a session and POST /acp/stream streams SSE.
    let session_wt = crate::test_board_fixture::init_board_workspace("srv-routes-session");
    let new_payload = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "session/new",
        "params": { "workspace": session_wt.to_string_lossy() }
    }))
    .unwrap();
    let authed_new = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_RPC_PATH}"),
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer acp-port-9094"),
        ],
        Some(&new_payload),
        20,
        0,
    )
    .unwrap();
    assert_eq!(authed_new.status, 200);
    let new_json: serde_json::Value =
        serde_json::from_str(&authed_new.into_utf8(1_000_000).unwrap()).unwrap();
    let sid = new_json["result"]["session_id"].as_str().unwrap();

    let stream_payload = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "session/prompt",
        "params": {
            "session_id": sid,
            "prompt": [{ "type": "text", "text": "acp over 9094" }]
        }
    }))
    .unwrap();
    let stream_resp = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_STREAM_PATH}"),
        &[
            ("Content-Type", "application/json"),
            ("Authorization", "Bearer acp-port-9094"),
        ],
        Some(&stream_payload),
        20,
        0,
    )
    .unwrap();
    assert_eq!(stream_resp.status, 200);
    let sse_text = stream_resp.into_utf8(1_000_000).unwrap();
    assert!(sse_text.contains("event: acp.turn.event"), "{sse_text}");
    assert!(sse_text.contains("event: acp.turn.result"), "{sse_text}");
    // The turn ran as a GAWD mission, not core's echo executor.
    assert!(
        sse_text.contains("gawd mission: acp over 9094"),
        "{sse_text}"
    );
    assert!(!sse_text.contains("acp-ack["), "{sse_text}");
}

#[test]
fn acp_sse_dispatch_delivers_lifecycle_and_delta_before_the_turn_finishes() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let engine = Arc::new(AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(PausingAcp {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }),
    ));
    let session = engine.handle_jsonrpc_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {}
        }))
        .unwrap(),
    );
    let session_id = serde_json::from_slice::<serde_json::Value>(&session).unwrap()["result"]
        ["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/prompt",
        "params": {"session_id": session_id, "prompt": "stream this"}
    }))
    .unwrap();
    let (frames_tx, frames_rx) = std::sync::mpsc::channel();
    let worker_engine = Arc::clone(&engine);
    let worker = thread::spawn(move || {
        let mut send = |frame: String| frames_tx.send(frame).unwrap();
        worker_engine
            .handle_sse_stream_bytes_with_session_header_and(&body, None, &mut send)
            .unwrap();
    });

    let first = frames_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("turn_started arrives before the mission blocks");
    assert!(first.contains("turn_started"), "{first}");
    let deadline = Instant::now() + Duration::from_secs(1);
    while !entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "mission did not enter its pause");
        thread::yield_now();
    }
    let delta = frames_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("message_delta arrives before the mission is released");
    assert!(delta.contains("message_delta"), "{delta}");
    assert!(delta.contains("early"), "{delta}");

    release.store(true, Ordering::Release);
    let mut result = String::new();
    while !result.contains("acp.turn.result") {
        result = frames_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("turn result arrives after release");
    }
    assert!(result.contains("done"), "{result}");
    worker.join().unwrap();
}

#[test]
fn acp_routes_propagate_and_enforce_acp_session_id_header() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve_shout(&runtime);

    let new_payload = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "session/new",
        "params": {}
    }))
    .unwrap();
    let created = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_RPC_PATH}"),
        &[("Content-Type", "application/json")],
        Some(&new_payload),
        20,
        0,
    )
    .unwrap();
    assert_eq!(created.status, 200);
    let created_json: serde_json::Value =
        serde_json::from_str(&created.into_utf8(1_000_000).unwrap()).unwrap();
    let sid = created_json["result"]["session_id"].as_str().unwrap();

    // 1. POST /acp omits session_id in params, bound via HTTP Acp-Session-Id header.
    let prompt_no_sid = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/prompt",
        "params": { "prompt": "header-bound rpc turn" }
    }))
    .unwrap();
    let rpc_resp = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_RPC_PATH}"),
        &[
            ("Content-Type", "application/json"),
            (ACP_SESSION_ID_HEADER, sid),
        ],
        Some(&prompt_no_sid),
        20,
        0,
    )
    .unwrap();
    assert_eq!(rpc_resp.status, 200);
    let rpc_json: serde_json::Value =
        serde_json::from_str(&rpc_resp.into_utf8(1_000_000).unwrap()).unwrap();
    assert_eq!(rpc_json["result"]["session_id"], sid, "{rpc_json}");
    assert_eq!(rpc_json["result"]["stop_reason"], "end_turn", "{rpc_json}");

    // 2. POST /acp rejects mismatched Acp-Session-Id header vs JSON-RPC params.session_id.
    let prompt_conflict = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "session/prompt",
        "params": { "session_id": "acp-sess-other", "prompt": "conflict" }
    }))
    .unwrap();
    let rpc_conflict = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_RPC_PATH}"),
        &[
            ("Content-Type", "application/json"),
            (ACP_SESSION_ID_HEADER, sid),
        ],
        Some(&prompt_conflict),
        20,
        0,
    )
    .unwrap();
    let rpc_conflict_json: serde_json::Value =
        serde_json::from_str(&rpc_conflict.into_utf8(1_000_000).unwrap()).unwrap();
    assert_eq!(
        rpc_conflict_json["error"]["code"], JSONRPC_INVALID_PARAMS,
        "{rpc_conflict_json}"
    );

    // 3. POST /acp/stream binds session_id from Acp-Session-Id header when omitted from params.
    let sse_no_sid = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "session/prompt",
        "params": { "prompt": "header-bound sse turn" }
    }))
    .unwrap();
    let sse_resp = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_STREAM_PATH}"),
        &[
            ("Content-Type", "application/json"),
            (ACP_SESSION_ID_HEADER, sid),
        ],
        Some(&sse_no_sid),
        20,
        0,
    )
    .unwrap();
    assert_eq!(sse_resp.status, 200);
    let sse_text = sse_resp.into_utf8(1_000_000).unwrap();
    assert!(sse_text.contains("event: acp.turn.result"), "{sse_text}");
    assert!(sse_text.contains(sid), "{sse_text}");
    assert!(sse_text.contains("header-bound rpc turn"), "{sse_text}");
    assert!(sse_text.contains("header-bound sse turn"), "{sse_text}");
    assert!(sse_text.contains("gawd mission:"), "{sse_text}");

    // 4. POST /acp/stream rejects mismatched Acp-Session-Id header vs JSON-RPC params.session_id.
    let sse_conflict = susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{ACP_STREAM_PATH}"),
        &[
            ("Content-Type", "application/json"),
            (ACP_SESSION_ID_HEADER, sid),
        ],
        Some(&prompt_conflict),
        20,
        0,
    )
    .unwrap();
    assert_eq!(sse_conflict.status, 200);
    let sse_conflict_json: serde_json::Value =
        serde_json::from_str(&sse_conflict.into_utf8(1_000_000).unwrap()).unwrap();
    assert_eq!(
        sse_conflict_json["error"]["code"], JSONRPC_INVALID_PARAMS,
        "{sse_conflict_json}"
    );
}
