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
    get, header, post, Before, Body, Guard, HeaderMap, HeaderValue, MethodRouter, Negotiate, Peer,
    Request, Response, Router, StatusCode,
};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

use crate::executor::GawdA2AExecutor;
use crate::protocol::{AgentCard, AgentExecutor, Handler, WELL_KNOWN_AGENT_CARD_PATH};
use crate::susi_core::acp::{
    AcpAdmissionPolicy, AcpEngine, AcpTurnExecutor, ACP_RPC_PATH, ACP_STREAM_PATH,
    WELL_KNOWN_ACP_PATH,
};
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

/// The reply when an ACP turn's blocking thread itself failed (a panic).
fn acp_internal_error() -> Vec<u8> {
    br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"ACP turn failed"}}"#.to_vec()
}
fn acp_discovery(acp: &AcpEngine) -> MethodRouter {
    let body = Arc::new(serde_json::to_string(&acp.discovery_document()).unwrap_or_default());
    get(move |_: Request| {
        let body = Arc::clone(&body);
        async move {
            let mut response = Response::new(Body::from(body.as_ref().clone()));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
                .headers_mut()
                .insert("access-control-allow-origin", HeaderValue::from_static("*"));
            response
        }
    })
}

/// The A2A and ACP routes for `executor`, with public discovery and guarded
/// task/session routes, served by [`serve`].
fn app(
    executor: Arc<dyn AgentExecutor>,
    acp_executor: Arc<dyn AcpTurnExecutor>,
    card: AgentCard,
    store: Arc<TaskStore>,
    verifier: Verifier,
) -> Router {
    let handler = Arc::new(Handler::new(executor, card.clone(), store));
    let acp = Arc::new(AcpEngine::new(AcpAdmissionPolicy::default(), acp_executor));
    let acp_rpc = Arc::clone(&acp);
    let acp_sse = Arc::clone(&acp);
    crate::protocol::router(handler)
        .route(
            ACP_RPC_PATH,
            post(move |request: Request| {
                let engine = Arc::clone(&acp_rpc);
                async move {
                    // A turn runs a whole GAWD mission: keep it off the worker.
                    let reply = tokio::task::spawn_blocking(move || {
                        engine.handle_jsonrpc_bytes(&request.body)
                    })
                    .await
                    .unwrap_or_else(|_| acp_internal_error());
                    let mut response = Response::new(Body::from(reply));
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    response
                }
            }),
        )
        .route(
            ACP_STREAM_PATH,
            post(move |request: Request| {
                let engine = Arc::clone(&acp_sse);
                async move {
                    if let Err(err_json) = engine.validate_sse_stream_bytes(&request.body) {
                        let mut response = Response::new(Body::from(err_json));
                        response.headers_mut().insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        );
                        return response;
                    }
                    let (tx, rx) = mpsc::channel(16);
                    let body = request.body;
                    std::mem::drop(tokio::task::spawn_blocking(move || {
                        let mut send_frame = |frame: String| {
                            let _ = tx.blocking_send(frame.into_bytes());
                        };
                        if let Err(err_json) = engine
                            .handle_sse_stream_bytes_with_session_header_and(
                                &body,
                                None,
                                &mut send_frame,
                            )
                        {
                            let frame = format!(
                                "event: acp.turn.error\ndata: {}\n\n",
                                String::from_utf8_lossy(&err_json)
                            );
                            let _ = tx.blocking_send(frame.into_bytes());
                        }
                    }));
                    let mut response = Response::new(Body::Stream(rx));
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    response
                        .headers_mut()
                        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                    response
                }
            }),
        )
        .guard_all(Some(MAX_AUTH_BODY_BYTES), auth_guard(verifier))
        .route_public(
            WELL_KNOWN_AGENT_CARD_PATH,
            crate::protocol::agent_card(&card),
        )
        .route_public(WELL_KNOWN_ACP_PATH, acp_discovery(&acp))
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
/// `verifier` decides admission for every non-card route. Blocks the calling
/// thread for the life of the listeners — spawn it the
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
        let app = app(
            Arc::new(executor),
            Arc::new(crate::executor::GawdAcpExecutor::new()),
            card,
            store,
            verifier,
        );
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
#[path = "tests/server_routes.rs"]
mod tests;
