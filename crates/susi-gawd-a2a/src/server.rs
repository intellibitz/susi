//! A2A HTTP surface: JSON-RPC (`/`), SSE (`/stream`), and the agent card at
//! `/.well-known/agent-card.json` — the in-tree A2A protocol
//! ([`crate::protocol`]).
//!
//! The daemon binds the canonical `A2A_HTTP` host-contract port and hands the
//! listener here; this module owns nothing but the accept loop. Authorization
//! is delegated to the caller's [`Verifier`] — the daemon wires in the same
//! zero-trust NetGuard policy as the other host-contract surfaces (bearer
//! token or a roster-bound member signature over the exact body bytes). The
//! agent card is the only unauthenticated route, matching A2A discovery
//! convention.

use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::Arc;
use susi_http_transport::server::{
    header, Before, Body, Guard, HeaderMap, HeaderValue, Negotiate, Peer, Request, Response,
    Router, StatusCode,
};
use tokio_rustls::TlsAcceptor;

use crate::executor::GawdA2AExecutor;
use crate::protocol::{AgentCard, AgentExecutor, Handler, WELL_KNOWN_AGENT_CARD_PATH};
use crate::task_store::TaskStore;

use susi_http_transport::dual_transport::negotiate_blocking;

/// Concurrent in-flight HTTP requests. Each accepted task can own a fleet
/// thread (see `run_fleet`), so an unbounded accept queue is a thread-spawn
/// DoS surface even with `MAX_INFLIGHT_REQUESTS` gating execution.
const MAX_CONCURRENT_REQUESTS: usize = 64;

/// Request bodies buffered for signature verification are bounded — the v2
/// signature binds the exact body, so the whole body must be read before
/// authorization, but an unbounded buffer is a memory-DoS surface.
const MAX_AUTH_BODY_BYTES: usize = 1024 * 1024;

/// Everything an authorization policy needs to decide a request: the peer's
/// TCP address (membership binding), method/path, all headers (bearer +
/// `x-susi-*` signature headers), and the exact buffered body bytes (v2
/// signatures cover their SHA-256).
pub struct VerifierContext<'a> {
    pub peer: IpAddr,
    pub method: &'a str,
    pub path: &'a str,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

/// Authorization policy the daemon injects: given the request context,
/// return whether it is admitted. Invoked for every non-card route.
pub type Verifier = Arc<dyn for<'a> Fn(&VerifierContext<'a>) -> bool + Send + Sync>;

/// RFC 9110 §15.5.2: a 401 MUST carry a challenge naming the scheme the
/// client should retry with.
fn unauthorized() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"susi-a2a\""),
    );
    response
}

/// The verifier as the guard in front of every route but the card: it sees
/// the whole body (at most [`MAX_AUTH_BODY_BYTES`], else `413`).
fn auth_guard(verifier: Verifier) -> Guard {
    Arc::new(move |request: &Request| {
        let ctx = VerifierContext {
            peer: request
                .peer
                .map_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), |peer| peer.ip()),
            method: &request.method,
            path: &request.path,
            headers: &request.headers,
            body: &request.body,
        };
        (!(verifier)(&ctx)).then(unauthorized)
    })
}

/// Liveness is public on every contract port (README, "Host contract"):
/// `GET /health` is answered ahead of the guard, its body unread.
fn health_first() -> Before {
    Arc::new(|method: &str, path: &str| (method == "GET" && path == "/health").then(health))
}

fn health() -> Response {
    let mut response = Response::new(Body::from(r#"{"status":"ok"}"#));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// The A2A routes for `executor` behind the authorization guard and the
/// concurrency bound — the stack [`serve`] runs. The agent card is public
/// discovery metadata: its route is mounted after the guard, so it is not
/// behind it; every task-bearing route (JSON-RPC, SSE) and every unknown
/// path is.
fn app(
    executor: Arc<dyn AgentExecutor>,
    card: AgentCard,
    store: Arc<TaskStore>,
    verifier: Verifier,
) -> Router {
    let handler = Arc::new(Handler::new(executor, card.clone(), store));
    crate::protocol::router(handler)
        .guard_all(Some(MAX_AUTH_BODY_BYTES), auth_guard(verifier))
        .route_public(
            WELL_KNOWN_AGENT_CARD_PATH,
            crate::protocol::agent_card(&card),
        )
        .before(health_first())
        .concurrency_limit(MAX_CONCURRENT_REQUESTS)
}

/// Each accepted socket: refused when the privacy mode refuses its host,
/// otherwise plain HTTP or TLS by its first byte. Off-host plaintext is
/// always refused (`require_tls_remote`); loopback is always exempt
/// (internal `http://127.0.0.1` callers).
fn negotiate(tls: Option<TlsAcceptor>, require_tls_remote: bool) -> Negotiate {
    Arc::new(move |stream, addr: SocketAddr| {
        let remote = !addr.ip().to_canonical().is_loopback();
        if crate::susi_core::mac_policy::refuses_remote_client(addr.ip()) {
            return None;
        }
        negotiate_blocking(stream, remote, tls.as_ref(), require_tls_remote, "[A2A]")
    })
}

/// Serve the A2A protocol on already-bound TCP listeners (at most two:
/// loopback plus a specific external bind), each serving plain HTTP and,
/// when `tls` is given, TLS on the platform stack (`native-tls`, the
/// acceptor's identity; it should offer only `http/1.1` by ALPN, the one
/// protocol this server speaks).
///
/// `verifier` decides admission for every non-card route (see above).
///
/// Blocks the calling thread for the life of the listeners — spawn it the
/// same way the GMCP/GEMI servers are spawned. The executor's fleet calls
/// run on dedicated OS threads (never on the server's runtime): the fleet's
/// provider dispatch owns a `current_thread` runtime and `block_on`s it,
/// which panics inside any enclosing tokio runtime.
pub fn serve(
    listeners: Vec<TcpListener>,
    verifier: Verifier,
    tls: Option<TlsAcceptor>,
    require_tls_remote: bool,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let executor = GawdA2AExecutor::new();
        let mut card = executor.agent_card();
        // Advertise the absolute endpoint — remote agents need a dialable URL
        // for follow-up JSON-RPC calls, not the relative forms the card
        // defaults to. https when TLS is configured (both protocols are
        // served on the port; the card should point at the safer one), and
        // the non-loopback socket when one is bound. A wildcard bind
        // (0.0.0.0/::) is not dialable — resolve it to the host's outbound
        // address instead.
        let scheme = if tls.is_some() { "https" } else { "http" };
        let advertise = listeners
            .iter()
            .filter_map(|l| l.local_addr().ok())
            .find(|a| !a.ip().is_loopback())
            .or_else(|| listeners.first().and_then(|l| l.local_addr().ok()));
        if let Some(addr) = advertise {
            let host = if addr.ip().is_unspecified() {
                // UDP "connect" sends no packets — it just resolves which
                // local address the route would use. TEST-NET-3 is
                // deliberately unreachable; only the source addr matters.
                std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
                    .and_then(|s| {
                        s.connect((std::net::Ipv4Addr::new(203, 0, 113, 1), 1))?;
                        s.local_addr()
                    })
                    .map(|a| a.ip())
                    .unwrap_or_else(|_| addr.ip())
            } else {
                addr.ip()
            };
            for interface in &mut card.supported_interfaces {
                interface.url = format!("{scheme}://{}:{}{}", host, addr.port(), interface.url);
            }
        }
        // Retention is bounded: a reaper evicts the oldest terminal tasks.
        // Only the JSON-RPC binding (with SSE) is served, and the card
        // advertises only what is served.
        let store = Arc::new(TaskStore::new());
        tokio::spawn(crate::task_store::reaper(Arc::clone(&store)));
        let app = app(Arc::new(executor), card, store, verifier);
        eprintln!(
            "[A2A] HTTP available: JSON-RPC /, SSE /stream, card /.well-known/agent-card.json"
        );
        susi_http_transport::server::serve_std(
            listeners,
            app,
            Peer::Pass,
            Some(negotiate(tls, require_tls_remote)),
            std::future::pending(),
        )
        .await
    })
}

/// `app` on a loopback port of its own, served on `runtime` (tests).
#[cfg(test)]
pub(crate) fn serve_on_loopback(runtime: &tokio::runtime::Runtime, app: Router) -> SocketAddr {
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(susi_http_transport::server::serve(
            listener,
            app,
            Peer::Pass,
            std::future::pending(),
        ));
        addr
    })
}

#[cfg(test)]
#[path = "tests/wire_golden.rs"]
mod wire_golden;

#[cfg(test)]
#[path = "tests/handler_paths.rs"]
mod handler_paths;

#[cfg(test)]
mod tests {
    use crate::protocol::{
        AgentExecutor, EventQueue, Message, Part, PartContent, RequestContext, Result, Task,
        TaskState, TaskStatus,
    };
    use crate::susi_core::a2a_wire;
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
}
