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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
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

    async fn supervisor_status(header: Option<HeaderValue>) -> StatusCode {
        let app = Router::new()
            .route("/", get(|| async { "reached" }))
            .layer(axum::middleware::from_fn(supervisor));
        app.oneshot(request_with(header)).await.unwrap().status()
    }

    #[tokio::test]
    async fn supervisor_middleware_enforces_bearer_policy_on_layered_routes() {
        for header in [
            None,
            Some(HeaderValue::from_static("Basic dXNlcjpwYXNz")),
            Some(HeaderValue::from_static("Bearer")),
            Some(HeaderValue::from_static(
                "Bearer definitely-not-the-supervisor-token-9a7c2e",
            )),
            Some(HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap()),
        ] {
            let token_before = std::env::var("SUSI_HOST_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty());
            let status = supervisor_status(header.clone()).await;
            let token_after = std::env::var("SUSI_HOST_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty());
            if token_before.is_none() && token_after.is_none() {
                assert_eq!(status, StatusCode::OK, "open policy for {header:?}");
            } else if token_before.is_some() && token_after.is_some() {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "armed policy for {header:?}"
                );
            } else {
                assert!(
                    matches!(status, StatusCode::OK | StatusCode::UNAUTHORIZED),
                    "concurrent transition for {header:?}: {status}"
                );
            }
        }
    }

    #[tokio::test]
    async fn request_with_empty_or_whitespace_authorization_header_is_extracted_and_rejected_by_host_token(
    ) {
        assert_eq!(
            authorization(&request_with(Some(HeaderValue::from_static("")))),
            Some("")
        );
        assert_eq!(
            authorization(&request_with(Some(HeaderValue::from_static("   ")))),
            Some("   ")
        );

        for raw in ["", "   ", "Bearer ", "Bearer    "] {
            assert_eq!(
                host_token_status(Some(HeaderValue::from_static(raw))).await,
                StatusCode::UNAUTHORIZED,
                "expected 401 for {raw:?}"
            );
        }
    }

    #[test]
    fn request_with_none_has_no_authorization_and_root_uri_is_preserved() {
        let req = request_with(None);
        assert_eq!(req.uri().path(), "/");
        assert!(authorization(&req).is_none());
    }
}
