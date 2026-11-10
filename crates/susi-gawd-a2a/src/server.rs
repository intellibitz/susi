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

use axum::body::{to_bytes, Body};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

use crate::executor::GawdA2AExecutor;
use crate::protocol::{AgentCard, AgentExecutor, Handler};
use crate::task_store::TaskStore;

use susi_http_transport::dual_transport::{negotiate_transport, MaybeTls};

/// An axum `Listener` that serves both plain HTTP and TLS on each bound
/// socket by peeking at the first byte before deciding the transport.
/// `inner` is at most two sockets — loopback plus a specific external bind.
struct DualListener {
    inner: Vec<tokio::net::TcpListener>,
    tls: Option<TlsAcceptor>,
    require_tls_remote: bool,
}

impl axum::serve::Listener for DualListener {
    type Io = MaybeTls;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let accepted = if self.inner.len() > 1 {
                tokio::select! {
                    r = self.inner[0].accept() => r,
                    r = self.inner[1].accept() => r,
                }
            } else {
                self.inner[0].accept().await
            };
            let (stream, addr) = match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("[A2A] Accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    continue;
                }
            };
            // Off-host plaintext is always refused (require_tls_remote);
            // loopback is always exempt (internal http://127.0.0.1 callers).
            // host_only serves this machine alone, whatever the bind.
            let remote = !addr.ip().to_canonical().is_loopback();
            if crate::susi_core::mac_policy::refuses_remote_client(addr.ip()) {
                continue;
            }
            if let Some(io) = negotiate_transport(
                stream,
                remote,
                self.tls.as_ref(),
                self.require_tls_remote,
                "[A2A]",
            )
            .await
            {
                return (io, addr);
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner[0].local_addr()
    }
}

/// The agent card is public discovery metadata; every task-bearing route
/// (JSON-RPC, REST, SSE) requires the bearer token.
const PUBLIC_PATH: &str = "/.well-known/agent-card.json";

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
    pub headers: &'a axum::http::HeaderMap,
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
        axum::http::header::WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer realm=\"susi-a2a\""),
    );
    response
}

async fn auth_guard(
    axum::extract::State(verifier): axum::extract::State<Verifier>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if req.uri().path() == PUBLIC_PATH {
        return Ok(next.run(req).await);
    }
    // Liveness is public on every contract port (README, "Host contract").
    if req.uri().path() == "/health" && req.method() == axum::http::Method::GET {
        return Ok(health());
    }
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, MAX_AUTH_BODY_BYTES)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let ctx = VerifierContext {
        peer,
        method: &method,
        path: &path,
        headers: &parts.headers,
        body: &bytes,
    };
    if !(verifier)(&ctx) {
        return Ok(unauthorized());
    }
    Ok(next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await)
}

fn health() -> Response {
    let mut response = Response::new(Body::from(r#"{"status":"ok"}"#));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}

/// The A2A routes for `executor` behind the authorization guard and the
/// concurrency bound — the stack [`serve`] runs.
fn app(
    executor: Arc<dyn AgentExecutor>,
    card: AgentCard,
    store: Arc<TaskStore>,
    verifier: Verifier,
) -> axum::Router {
    let handler = Arc::new(Handler::new(executor, card.clone(), store));
    crate::protocol::router(handler, &card)
        .layer(axum::middleware::from_fn_with_state(verifier, auth_guard))
        .layer(tower::limit::ConcurrencyLimitLayer::new(
            MAX_CONCURRENT_REQUESTS,
        ))
}

/// Serve the A2A protocol on an already-bound TCP listener.
///
/// `verifier` decides admission for every non-card route (see above).
///
/// Blocks the calling thread for the life of the listener — spawn it the same
/// way the GMCP/GEMI servers are spawned. The executor's fleet calls run on
/// dedicated OS threads (never on the axum runtime): the fleet's provider
/// dispatch owns a `current_thread` runtime and `block_on`s it, which panics
/// inside any enclosing tokio runtime.
pub fn serve(
    listeners: Vec<TcpListener>,
    verifier: Verifier,
    tls: Option<TlsAcceptor>,
    require_tls_remote: bool,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let mut inner = Vec::with_capacity(listeners.len());
        for listener in listeners {
            listener.set_nonblocking(true)?;
            inner.push(tokio::net::TcpListener::from_std(listener)?);
        }
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
        let advertise = inner
            .iter()
            .filter_map(|l| l.local_addr().ok())
            .find(|a| !a.ip().is_loopback())
            .or_else(|| inner.first().and_then(|l| l.local_addr().ok()));
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
        // `tap_io` wrap is what makes `ConnectInfo<SocketAddr>` resolve on a
        // custom listener: axum provides `Connected` impls for
        // `IncomingStream<TapIo<L,_>>`, not for arbitrary listeners.
        use axum::serve::ListenerExt;
        let dual = DualListener {
            inner,
            tls,
            require_tls_remote,
        }
        .tap_io(|_| {});
        // Retention is bounded: a reaper evicts the oldest terminal tasks.
        // Only the JSON-RPC binding (with SSE) is served, and the card
        // advertises only what is served.
        let store = Arc::new(TaskStore::new());
        tokio::spawn(crate::task_store::reaper(Arc::clone(&store)));
        let app = app(Arc::new(executor), card, store, verifier);
        eprintln!(
            "[A2A] HTTP available: JSON-RPC /, SSE /stream, card /.well-known/agent-card.json"
        );
        axum::serve(
            dual,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
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
        runtime.block_on(async {
            let card = crate::executor::GawdA2AExecutor::new().agent_card();
            let app = super::app(
                Arc::new(Shout),
                card,
                Arc::new(crate::task_store::TaskStore::new()),
                Arc::new(|_: &super::VerifierContext| true),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
            });
            addr
        })
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
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[axum::http::header::WWW_AUTHENTICATE],
            "Bearer realm=\"susi-a2a\""
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
