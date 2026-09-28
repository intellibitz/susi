//! The two leaf-service bearer policies, as axum middleware.

use axum::extract::Request;
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

fn authorization(req: &Request) -> Option<&str> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
}

/// Host token file policy (fails closed): config and sandbox routes read or
/// write the user's substrate, and loopback is shared by all local users.
pub(crate) async fn host_token(req: Request, next: Next) -> Response {
    if susi_paths::bearer_authorized(authorization(&req)) {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Supervisor env-token policy (open when `SUSI_HOST_TOKEN` is unset).
pub(crate) async fn supervisor(req: Request, next: Next) -> Response {
    if susi_paths::supervisor_bearer_authorized(authorization(&req)) {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}
