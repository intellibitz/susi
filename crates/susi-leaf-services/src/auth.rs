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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::HeaderValue;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn request_with(header: Option<HeaderValue>) -> Request {
        let mut builder = Request::builder().uri("/");
        if let Some(value) = header {
            builder = builder.header(AUTHORIZATION, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn authorization_extracts_only_printable_ascii_header_values() {
        assert_eq!(authorization(&request_with(None)), None);
        assert_eq!(
            authorization(&request_with(Some(HeaderValue::from_static(
                "Bearer tok-123"
            )))),
            Some("Bearer tok-123")
        );
        // Opaque (non-ASCII) bytes are a valid HeaderValue but never a bearer.
        let opaque = HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap();
        assert_eq!(authorization(&request_with(Some(opaque))), None);
    }

    async fn host_token_status(header: Option<HeaderValue>) -> StatusCode {
        let app = Router::new()
            .route("/", get(|| async { "reached" }))
            .layer(axum::middleware::from_fn(host_token));
        app.oneshot(request_with(header)).await.unwrap().status()
    }

    // The host policy fails closed. These inputs can never authorize, whatever
    // token file the process-global substrate root happens to point at.
    #[tokio::test]
    async fn host_token_rejects_missing_malformed_and_wrong_credentials() {
        assert_eq!(host_token_status(None).await, StatusCode::UNAUTHORIZED);
        for value in [
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
            HeaderValue::from_static("Bearer"),
            HeaderValue::from_static("bearer lowercase-scheme"),
            HeaderValue::from_static("Bearer definitely-not-the-host-token-4f1c9e"),
            HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap(),
        ] {
            assert_eq!(
                host_token_status(Some(value.clone())).await,
                StatusCode::UNAUTHORIZED,
                "{value:?}"
            );
        }
    }
}
