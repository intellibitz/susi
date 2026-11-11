//! The two leaf-service bearer policies for the shared std HTTP server.

use susi_http_transport::server::{Guard, IntoResponse, Request, Response, Router, StatusCode};

fn authorization(req: &Request) -> Option<&str> {
    req.headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
}

/// Host token file policy (fails closed): config and sandbox routes read or
/// write the user's substrate, and loopback is shared by all local users.
pub(crate) fn host_token(req: &Request) -> Option<Response> {
    if susi_paths::bearer_authorized(authorization(req)) {
        None
    } else {
        Some(StatusCode::UNAUTHORIZED.into_response())
    }
}

/// Supervisor env-token policy (open when `SUSI_HOST_TOKEN` is unset).
pub(crate) fn supervisor(req: &Request) -> Option<Response> {
    if susi_paths::supervisor_bearer_authorized(authorization(req)) {
        None
    } else {
        Some(StatusCode::UNAUTHORIZED.into_response())
    }
}

/// Attach a head-only guard to every route and to the router fallback. The
/// authorization policies do not inspect a body, so the request body remains
/// available to the route handler exactly as it was on the axum stack.
pub(crate) fn guard_all(router: Router, guard: Guard) -> Router {
    router.guard_all(None, guard)
}

#[cfg(test)]
// Test fixtures are intentionally fail-fast; a malformed request identifies the broken contract.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use susi_http_transport::server::{get, HeaderMap, HeaderValue};

    fn request_with(header: Option<HeaderValue>) -> Request {
        let mut headers = HeaderMap::new();
        if let Some(value) = header {
            headers.insert("authorization", value);
        }
        Request {
            method: "GET".into(),
            path: "/".into(),
            query: String::new(),
            headers,
            body: Vec::new(),
            peer: None,
        }
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
        let opaque = HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap();
        assert_eq!(authorization(&request_with(Some(opaque))), None);
    }

    async fn guarded_status(guard: Guard, header: Option<HeaderValue>) -> StatusCode {
        Router::new()
            .route("/", get(|_| async { "reached" }))
            .guard_all(None, guard)
            .call(request_with(header))
            .await
            .status()
    }

    #[tokio::test]
    async fn host_token_rejects_missing_malformed_and_wrong_credentials() {
        let guard: Guard = Arc::new(host_token);
        assert_eq!(
            guarded_status(guard.clone(), None).await,
            StatusCode::UNAUTHORIZED
        );
        for value in [
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
            HeaderValue::from_static("Bearer"),
            HeaderValue::from_static("bearer lowercase-scheme"),
            HeaderValue::from_static("Bearer definitely-not-the-host-token-4f1c9e"),
            HeaderValue::from_bytes(b"Bearer \xff\xfe").unwrap(),
        ] {
            assert_eq!(
                guarded_status(guard.clone(), Some(value)).await,
                StatusCode::UNAUTHORIZED
            );
        }
    }

    #[tokio::test]
    async fn supervisor_policy_enforces_bearer_when_armed_and_is_open_otherwise() {
        let guard: Guard = Arc::new(supervisor);
        let before = std::env::var("SUSI_HOST_TOKEN").ok();
        std::env::remove_var("SUSI_HOST_TOKEN");
        assert_eq!(guarded_status(guard.clone(), None).await, StatusCode::OK);
        std::env::set_var("SUSI_HOST_TOKEN", "supervisor-test-token");
        assert_eq!(
            guarded_status(guard.clone(), None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            guarded_status(
                guard,
                Some(HeaderValue::from_static("Bearer supervisor-test-token")),
            )
            .await,
            StatusCode::OK
        );
        match before {
            Some(value) => std::env::set_var("SUSI_HOST_TOKEN", value),
            None => std::env::remove_var("SUSI_HOST_TOKEN"),
        }
    }

    #[test]
    fn empty_authorization_is_extracted_but_never_authorizes() {
        assert_eq!(
            authorization(&request_with(Some(HeaderValue::from_static("")))),
            Some("")
        );
        assert_eq!(
            host_token(&request_with(Some(HeaderValue::from_static(" "))))
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
