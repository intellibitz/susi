//! The A2A JSON-RPC binding over HTTP: the agent card at
//! `/.well-known/agent-card.json`, JSON-RPC 2.0 on `POST /`, and the
//! streaming methods as Server-Sent Events on `POST /stream`.
//!
//! Method names: the A2A 1.0 names the card advertises (`SendMessage`,
//! `GetTask`, ...) and the slash names of earlier revisions
//! (`message/send`, `tasks/get`, ...) reach the same handler.
//!
//! Replies keep the shapes clients already parse (pinned by
//! `tests/golden/a2a_wire.json`): a malformed envelope is -32700 with no
//! `id`; an unknown method -32601, a streaming method on `/` -32600, and
//! missing or undecodable params -32602, each with the request's `id`; a
//! refusal by the handler is -32603 carrying its text with a `null` id on
//! `/`, and its own A2A code (-32001 task not found, ...) on `/stream`.
//! Results keep their declared member order; error replies and stream
//! events are written through a JSON value (members in name order).

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use hyper::body::Frame;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::error::{
    A2AError, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use super::handler::{Handler, TaskEvents};
use super::types::{AgentCard, GetExtendedAgentCardRequest, Task};

/// Idle interval after which an SSE stream sends a keep-alive comment.
const SSE_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Well-known path of the public agent card.
pub const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

/// The reply to a malformed envelope on `/`: no `id` member at all.
const PARSE_ERROR_REPLY: &str =
    r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Invalid JSON payload"}}"#;

#[derive(Clone)]
struct ServerState {
    handler: Arc<Handler>,
    /// The card, serialized once.
    card: Arc<String>,
}

/// The A2A routes for `handler`, advertising `card`.
pub fn router(handler: Arc<Handler>, card: &AgentCard) -> Router {
    let state = ServerState {
        handler,
        card: Arc::new(serde_json::to_string(card).unwrap_or_default()),
    };
    Router::new()
        .route(WELL_KNOWN_AGENT_CARD_PATH, get(agent_card))
        .route("/", post(json_rpc))
        .route("/stream", post(sse))
        .with_state(state)
}

fn response(content_type: &'static str, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn json_ok(body: String) -> Response {
    response("application/json", Body::from(body))
}

/// Public discovery metadata, readable cross-origin.
async fn agent_card(State(state): State<ServerState>) -> Response {
    let mut response = json_ok(state.card.as_ref().clone());
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// A JSON-RPC request id: a string or an integer (`null`, fractions and
/// out-of-range numbers make the envelope malformed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum RequestId {
    String(String),
    Number(i64),
}

/// The JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    id: RequestId,
    method: String,
    params: Option<Value>,
}

/// `message/send`'s result: the task (this agent never answers with a
/// bare message).
#[derive(Serialize)]
struct TaskResult {
    task: Task,
}

/// A2A 1.0 method names (A2A 1.0 section 9.1) and the slash names they
/// alias.
fn canonical_method(method: &str) -> Option<&'static str> {
    Some(match method {
        "SendMessage" => "message/send",
        "SendStreamingMessage" => "message/stream",
        "GetTask" => "tasks/get",
        "ListTasks" => "tasks/list",
        "CancelTask" => "tasks/cancel",
        "SubscribeToTask" => "tasks/resubscribe",
        "CreateTaskPushNotificationConfig" => "tasks/pushNotificationConfig/create",
        "GetTaskPushNotificationConfig" => "tasks/pushNotificationConfig/get",
        "ListTaskPushNotificationConfigs" => "tasks/pushNotificationConfig/list",
        "DeleteTaskPushNotificationConfig" => "tasks/pushNotificationConfig/delete",
        "GetExtendedAgentCard" => "agent/getAuthenticatedExtendedCard",
        _ => return None,
    })
}

/// Parse the envelope and resolve a 1.0 method name to its slash name.
/// A request naming a 1.0 method is read through a JSON value first (a
/// repeated member keeps its last value); under a slash name a repeated
/// member is a malformed envelope, as it always was.
fn parse_envelope(body: &str) -> Option<JsonRpcRequest> {
    let value = serde_json::from_str::<Value>(body).ok();
    let canonical = value
        .as_ref()
        .and_then(|v| v.get("method"))
        .and_then(Value::as_str)
        .and_then(canonical_method);
    match (canonical, value) {
        (Some(canonical), Some(value)) => {
            let mut request: JsonRpcRequest = serde_json::from_value(value).ok()?;
            request.method = canonical.to_string();
            Some(request)
        }
        _ => serde_json::from_str(body).ok(),
    }
}

/// Why a JSON-RPC call failed, and so how its error is written.
enum Failure {
    /// A protocol error answered with the request's id.
    Rpc(i32, String),
    /// A handler refusal (`/` reports these as -32603 with a null id).
    Handler(A2AError),
}

impl From<A2AError> for Failure {
    fn from(e: A2AError) -> Self {
        Self::Handler(e)
    }
}

fn params<T: serde::de::DeserializeOwned>(request: &JsonRpcRequest) -> Result<T, Failure> {
    let params = request
        .params
        .clone()
        .ok_or_else(|| Failure::Rpc(INVALID_PARAMS, "Missing params".into()))?;
    serde_json::from_value(params)
        .map_err(|e| Failure::Rpc(INVALID_PARAMS, format!("Invalid params: {e}")))
}

/// A result serialized in its declared member order.
fn to_json<T: Serialize>(value: &T) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|e| Failure::Handler(A2AError::Other(e.to_string())))
}

/// Run one request; the result is its serialized JSON.
async fn dispatch(handler: &Handler, request: &JsonRpcRequest) -> Result<String, Failure> {
    match request.method.as_str() {
        "message/send" => to_json(&TaskResult {
            task: handler.send(params(request)?).await?,
        }),
        "message/stream" | "tasks/resubscribe" => Err(Failure::Rpc(
            INVALID_REQUEST,
            "Streaming methods must be called via the SSE endpoint".into(),
        )),
        "tasks/get" => to_json(&handler.get(params(request)?)?),
        "tasks/list" => to_json(&handler.list(params(request)?)),
        "tasks/cancel" => to_json(&handler.cancel(params(request)?).await?),
        "tasks/pushNotificationConfig/create" => {
            to_json(&handler.create_push_config(params(request)?)?)
        }
        "tasks/pushNotificationConfig/get" => to_json(&handler.get_push_config(params(request)?)?),
        "tasks/pushNotificationConfig/list" => {
            to_json(&handler.list_push_configs(params(request)?)?)
        }
        "tasks/pushNotificationConfig/delete" => {
            handler.delete_push_config(params(request)?)?;
            Ok("null".to_string())
        }
        "agent/getAuthenticatedExtendedCard" => {
            params::<GetExtendedAgentCardRequest>(request)?;
            Err(A2AError::ExtendedCardNotConfigured.into())
        }
        method => Err(Failure::Rpc(
            METHOD_NOT_FOUND,
            format!("Method '{method}' not found"),
        )),
    }
}

/// `{"jsonrpc", "id", "error"}`, written through a JSON value.
fn error_reply(id: Option<&RequestId>, code: i32, message: String) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// `POST /`: one JSON-RPC request, one JSON reply.
async fn json_rpc(State(state): State<ServerState>, body: String) -> Response {
    let Some(request) = parse_envelope(&body) else {
        return json_ok(PARSE_ERROR_REPLY.to_string());
    };
    json_ok(match dispatch(&state.handler, &request).await {
        Ok(result) => format!(
            r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
            serde_json::to_string(&request.id).unwrap_or_default()
        ),
        Err(Failure::Rpc(code, message)) => error_reply(Some(&request.id), code, message),
        Err(Failure::Handler(e)) => error_reply(None, INTERNAL_ERROR, e.to_string()),
    })
}

/// `POST /stream`: `message/stream` or `tasks/resubscribe` as Server-Sent
/// Events, one JSON-RPC reply per task event; the stream ends after a
/// terminal or interrupted state. A request that cannot start a stream is
/// answered with one JSON reply instead.
async fn sse(State(state): State<ServerState>, body: String) -> Response {
    let Some(request) = parse_envelope(&body) else {
        return json_ok(error_reply(
            None,
            PARSE_ERROR,
            "Invalid JSON payload".into(),
        ));
    };
    let started = match request.method.as_str() {
        "message/stream" => {
            params(&request).and_then(|p| state.handler.stream(p).map_err(Failure::from))
        }
        "tasks/resubscribe" => {
            params(&request).and_then(|p| state.handler.resubscribe(p).map_err(Failure::from))
        }
        method => Err(Failure::Rpc(
            METHOD_NOT_FOUND,
            format!("Method '{method}' not found"),
        )),
    };
    let events = match started {
        Ok(events) => events,
        Err(Failure::Rpc(code, message)) => {
            return json_ok(error_reply(Some(&request.id), code, message))
        }
        Err(Failure::Handler(e)) => {
            return json_ok(error_reply(Some(&request.id), e.code(), e.to_string()))
        }
    };
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(forward(events, request.id, tx, Arc::clone(&state.handler)));
    let mut response = response(
        "text/event-stream",
        Body::new(SseBody {
            frames: rx,
            keep_alive: Box::pin(tokio::time::sleep(SSE_KEEP_ALIVE)),
        }),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Write each task event as an SSE `data:` frame until the stream is over
/// or the client went away.
async fn forward(
    mut events: TaskEvents,
    id: RequestId,
    tx: mpsc::Sender<Bytes>,
    handler: Arc<Handler>,
) {
    while let Some(event) = events.next().await {
        let (data, last) = match event {
            Ok(task) => {
                let last = task.status.state.is_terminal() || task.status.state.is_interrupted();
                let data = json!({"jsonrpc": "2.0", "id": id, "result": {"task": task}});
                (data, last)
            }
            Err(e) => (
                json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": e.code(), "message": e.to_string()}}),
                false,
            ),
        };
        let sent = tx.send(Bytes::from(format!("data: {data}\n\n"))).await;
        if sent.is_err() || last {
            break;
        }
    }
    handler.release(events);
}

/// An SSE body fed by [`forward`], with a keep-alive comment whenever the
/// stream idles for [`SSE_KEEP_ALIVE`].
struct SseBody {
    frames: mpsc::Receiver<Bytes>,
    keep_alive: Pin<Box<tokio::time::Sleep>>,
}

impl hyper::body::Body for SseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        let frame = match this.frames.poll_recv(cx) {
            Poll::Ready(Some(frame)) => frame,
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Pending => match this.keep_alive.as_mut().poll(cx) {
                Poll::Ready(()) => Bytes::from_static(b":\n\n"),
                Poll::Pending => return Poll::Pending,
            },
        };
        this.keep_alive
            .as_mut()
            .reset(tokio::time::Instant::now() + SSE_KEEP_ALIVE);
        Poll::Ready(Some(Ok(Frame::data(frame))))
    }
}
