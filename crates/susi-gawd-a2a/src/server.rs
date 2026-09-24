//! A2A HTTP surface: JSON-RPC (`/`), SSE (`/stream`), and the agent card at
//! `/.well-known/agent-card.json` — mounted from the ra2a `a2a_router`.
//!
//! The daemon binds the canonical `A2A_HTTP` host-contract port and hands the
//! listener here; this module owns nothing but the accept loop. Task routes
//! require the same `Authorization: Bearer` token as every other host-contract
//! surface — the agent card is the only unauthenticated route, matching A2A
//! discovery convention.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use ra2a::server::{a2a_router, ServerState};
use std::net::TcpListener;
use std::sync::Arc;
use susi_gawd_agents::GawdAgentFleet;

use crate::executor::GawdA2AExecutor;

/// The agent card is public discovery metadata; every task-bearing route
/// (JSON-RPC, REST, SSE) requires the bearer token.
const PUBLIC_PATH: &str = "/.well-known/agent-card.json";

async fn bearer_guard(
    State(expected): State<Arc<str>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if req.uri().path() == PUBLIC_PATH {
        return Ok(next.run(req).await);
    }
    let authorized = !expected.is_empty()
        && req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|token| token == expected.as_ref());
    if authorized {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Serve the A2A protocol on an already-bound TCP listener.
///
/// `bearer_token` is the daemon's `api_auth_token`; an empty token fails
/// closed (only the agent card is served).
///
/// Blocks the calling thread for the life of the listener — spawn it the same
/// way the GMCP/GEMI servers are spawned. The executor's fleet calls run on
/// dedicated OS threads (never on the axum runtime): the fleet's provider
/// dispatch owns a `current_thread` runtime and `block_on`s it, which panics
/// inside any enclosing tokio runtime.
pub fn serve(listener: TcpListener, bearer_token: String) -> std::io::Result<()> {
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
        let app = a2a_router(state).layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(bearer_token.as_str()),
            bearer_guard,
        ));
        eprintln!(
            "[A2A] HTTP available: JSON-RPC /, SSE /stream, card /.well-known/agent-card.json"
        );
        axum::serve(listener, app).await
    })
}
