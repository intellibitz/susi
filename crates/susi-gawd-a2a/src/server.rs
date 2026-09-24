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
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::Arc;
use susi_gawd_agents::GawdAgentFleet;

use crate::executor::GawdA2AExecutor;

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
        return Err(StatusCode::UNAUTHORIZED);
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
pub fn serve(listener: TcpListener, verifier: Verifier) -> std::io::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let executor = GawdA2AExecutor::new(Arc::new(GawdAgentFleet));
        let mut card = executor.agent_card();
        // Advertise the absolute endpoint — remote agents need a dialable URL
        // for follow-up JSON-RPC calls, not the relative forms the card
        // defaults to.
        if let Ok(addr) = listener.local_addr() {
            for interface in &mut card.supported_interfaces {
                interface.url = format!("http://{addr}{}", interface.url);
            }
        }
        let state = ServerState::from_executor(executor, card);
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
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    })
}
