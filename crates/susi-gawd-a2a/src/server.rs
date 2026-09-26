//! A2A HTTP surface: JSON-RPC (`/`), SSE (`/stream`), and the agent card at
//! `/.well-known/agent-card.json` — mounted from the ra2a `a2a_router`.
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
use ra2a::server::{a2a_router, ServerState};
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::Arc;
use susi_gawd_agents::GawdAgentFleet;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

use crate::executor::GawdA2AExecutor;

// Dual-protocol transport: the accept loop sniffs each connection's first
// byte. A TLS ClientHello (0x16) is served over TLS when an acceptor is
// configured; anything else is plain HTTP — internal `http://127.0.0.1`
// callers are unaffected. `require_tls_remote` drops non-TLS bytes from
// off-host peers; loopback plaintext is always allowed.
enum MaybeTls {
    Plain(tokio::net::TcpStream),
    Tls(Box<TlsStream<tokio::net::TcpStream>>),
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_shutdown(cx),
        }
    }
    fn poll_write_vectored(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => std::pin::Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Tls(s) => std::pin::Pin::new(&mut **s).poll_write_vectored(cx, bufs),
        }
    }
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Plain(s) => s.is_write_vectored(),
            Self::Tls(s) => s.is_write_vectored(),
        }
    }
}

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
            // Dual-protocol on every socket: a TLS ClientHello (0x16) is
            // upgraded when a cert is configured, anything else is plain
            // HTTP — so TLS-terminated-upstream deployments keep working.
            // Off-host plaintext is refused only under `https_only`;
            // loopback is always exempt (internal http://127.0.0.1 callers).
            let remote = !addr.ip().is_loopback();
            let mut probe = [0u8; 1];
            let is_tls = matches!(stream.peek(&mut probe).await, Ok(1) if probe[0] == 0x16);
            if is_tls {
                if let Some(acceptor) = &self.tls {
                    match acceptor.accept(stream).await {
                        Ok(s) => return (MaybeTls::Tls(Box::new(s)), addr),
                        Err(e) => {
                            eprintln!("[A2A] TLS handshake failed for {addr}: {e}");
                            continue;
                        }
                    }
                }
                continue;
            }
            if remote && self.require_tls_remote {
                continue;
            }
            return (MaybeTls::Plain(stream), addr);
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
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
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
        // Own the task store so retention is bounded: ra2a's default
        // InMemoryTaskStore grows forever, and its TaskVersion/GetTaskFuture
        // types aren't exported so a bounded TaskStore impl can't be written
        // — instead a reaper periodically evicts the oldest terminal tasks
        // through the exported list/delete surface.
        let task_store = Arc::new(ra2a::server::InMemoryTaskStore::new());
        let handler = ra2a::server::DefaultRequestHandler::new(executor, card.clone())
            .with_task_store(task_store.clone())
            .with_push_sender(Arc::new(ra2a::server::HttpPushSender::new()));
        let state = ServerState::new(Arc::new(handler), card);
        tokio::spawn(crate::task_store::reaper(task_store));
        // `a2a_router`, not `a2a_full_router`: the REST binding registers
        // `/tasks/{id}:cancel`-style paths that axum's router rejects with a
        // panic — and the release profile is `panic = "abort"`, which would
        // take the whole daemon down at startup. JSON-RPC + SSE is the
        // primary A2A binding anyway; the card advertises only what we serve.
        let app = a2a_router(state)
            .layer(axum::middleware::from_fn_with_state(verifier, auth_guard))
            .layer(tower::limit::ConcurrencyLimitLayer::new(
                MAX_CONCURRENT_REQUESTS,
            ));
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
mod tests {
    use crate::susi_core::a2a_wire;
    use ra2a::server::{AgentExecutor, Event, EventQueue, RequestContext};
    use ra2a::types::{Message, Part, PartContent, Task, TaskState, TaskStatus};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// Replies with the received text, uppercased, as a completed task.
    struct Shout;

    impl AgentExecutor for Shout {
        fn execute<'a>(
            &'a self,
            ctx: &'a RequestContext,
            queue: &'a EventQueue,
        ) -> Pin<Box<dyn Future<Output = ra2a::error::Result<()>> + Send + 'a>> {
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
                queue.send(Event::Task(task))?;
                Ok(())
            })
        }

        fn cancel<'a>(
            &'a self,
            _ctx: &'a RequestContext,
            _queue: &'a EventQueue,
        ) -> Pin<Box<dyn Future<Output = ra2a::error::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
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
    /// ra2a JSON-RPC router: the v1.0 part is decoded as text and the task
    /// result folds back into its reply.
    #[test]
    fn shared_message_send_round_trips_through_ra2a() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let addr = runtime.block_on(async {
            let card =
                crate::executor::GawdA2AExecutor::new(Arc::new(susi_gawd_agents::GawdAgentFleet))
                    .agent_card();
            let handler = ra2a::server::DefaultRequestHandler::new(Shout, card.clone());
            let app =
                ra2a::server::a2a_router(ra2a::server::ServerState::new(Arc::new(handler), card));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await });
            addr
        });
        let reply: serde_json::Value = ureq::post(format!("http://{addr}/"))
            .header("A2A-Version", a2a_wire::A2A_VERSION)
            .send_json(a2a_wire::message_send_request("ping"))
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        assert_eq!(
            a2a_wire::reply_summary(&reply).unwrap(),
            "task completed: PING",
            "{reply}"
        );
    }
}
