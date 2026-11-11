//! `susi-error`: `POST /log_error` (open, write-only) and
//! `GET /errors/recent?n=` (supervisor-token gated: history carries paths).

use std::sync::Arc;

use susi_http_transport::server::{
    get, post, Guard, IntoResponse, Json, Request, Response, Router, StatusCode,
};

#[derive(serde::Deserialize)]
struct RecentQuery {
    n: Option<usize>,
}

fn recent_query(request: &Request) -> Result<RecentQuery, Response> {
    let mut query = RecentQuery { n: None };
    for (name, value) in request.query_pairs() {
        if name == "n" {
            query.n = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| Request::query_rejection("n: invalid unsigned integer"))?,
            );
        }
    }
    Ok(query)
}

async fn log_error(request: Request) -> Response {
    let event = match request.json::<susi_error::PostedErrorEvent>() {
        Ok(event) => event,
        Err(response) => return response,
    };
    match susi_error::append_posted_event(event) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn recent_errors(request: Request) -> Response {
    let query = match recent_query(&request) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let entries = susi_error::recent_error_entries(query.n.unwrap_or(50));
    Json(serde_json::json!({ "returned": entries.len(), "entries": entries })).into_response()
}

pub(crate) fn router() -> Router {
    Router::new()
        .route(
            "/errors/recent",
            get(recent_errors).guard_head(Arc::new(crate::auth::supervisor) as Guard),
        )
        .route("/log_error", post(log_error))
}

#[cfg(test)]
// Test fixtures are intentionally fail-fast; a malformed request identifies the broken contract.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use susi_http_transport::server::{HeaderMap, HeaderValue};

    fn request(method: &str, uri: &str, body: &str, content_type: Option<&str>) -> Request {
        let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
        let mut headers = HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert("content-type", HeaderValue::from_str(content_type).unwrap());
        }
        Request {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
            peer: None,
        }
    }

    #[test]
    fn recent_query_n_is_optional_and_unsigned() {
        let some: RecentQuery = serde_json::from_value(serde_json::json!({"n": 10})).unwrap();
        assert_eq!(some.n, Some(10));
        let none: RecentQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(none.n, None);
        assert!(serde_json::from_value::<RecentQuery>(serde_json::json!({"n": -1})).is_err());
        assert!(serde_json::from_value::<RecentQuery>(serde_json::json!({"n": "ten"})).is_err());
    }

    async fn post(content_type: Option<&str>, body: &str) -> StatusCode {
        router()
            .call(request("POST", "/log_error", body, content_type))
            .await
            .status()
    }

    #[tokio::test]
    async fn log_error_rejects_malformed_payloads_before_touching_the_sink() {
        let json = Some("application/json");
        assert_eq!(post(json, "{not json").await, StatusCode::BAD_REQUEST);
        assert_eq!(post(json, "").await, StatusCode::BAD_REQUEST);
        assert_eq!(
            post(json, "[1, 2, 3]").await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            post(None, r#"{"message":"no content type"}"#).await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            post(Some("text/plain"), r#"{"message":"wrong type"}"#).await,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[tokio::test]
    async fn log_error_is_write_only() {
        let response = router().call(request("GET", "/log_error", "", None)).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn recent_errors_rejects_non_get_methods_and_serves_json_or_unauthorized_on_get() {
        for method in ["POST", "DELETE"] {
            let status = router()
                .call(request(method, "/errors/recent", "", None))
                .await
                .status();
            assert!(status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::UNAUTHORIZED);
        }

        let response = router()
            .call(request("GET", "/errors/recent?n=0", "", None))
            .await;
        let status = response.status();
        assert!(status == StatusCode::OK || status == StatusCode::UNAUTHORIZED);
        if status == StatusCode::OK {
            let bytes = match response.into_body() {
                susi_http_transport::server::Body::Full(bytes) => bytes,
                susi_http_transport::server::Body::Stream(_) => {
                    panic!("recent errors is not streamed")
                }
            };
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["returned"], 0);
            assert!(body["entries"].is_array());
        }
    }

    #[tokio::test]
    async fn unknown_error_service_route_returns_not_found() {
        let status = router()
            .call(request("GET", "/errors/unknown_subroute", "", None))
            .await
            .status();
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED);
    }
}
