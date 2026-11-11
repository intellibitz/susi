use crate::protocol::{
    AgentExecutor, EventQueue, Message, Part, PartContent, RequestContext, Result, Task, TaskState,
    TaskStatus,
};
use crate::susi_core::a2a_wire;
use crate::susi_core::acp::{
    ACP_DISCOVERY_SCHEMA, ACP_PROTOCOL_VERSION, ACP_RPC_PATH, ACP_STREAM_PATH, WELL_KNOWN_ACP_PATH,
};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

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

/// The production stack around `Shout`, admitting every caller, on a
/// loopback port.
fn serve_shout(runtime: &tokio::runtime::Runtime) -> SocketAddr {
    let card = crate::executor::GawdA2AExecutor::new().agent_card();
    let app = super::app(
        Arc::new(Shout),
        Arc::new(crate::executor::GawdAcpExecutor::with_runner(Arc::new(
            |prompt: &str| Ok(format!("gawd mission: {prompt}")),
        ))),
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
        Arc::new(crate::executor::GawdAcpExecutor::with_runner(Arc::new(
            |prompt: &str| Ok(format!("gawd mission: {prompt}")),
        ))),
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
    let new_payload = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "session/new",
        "params": { "workspace": "crates/susi-core" }
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
