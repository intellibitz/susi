//! `susi-error`: `POST /log_error` (open, write-only) and
//! `GET /errors/recent?n=` (supervisor-token gated: history carries paths).

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

#[derive(serde::Deserialize)]
struct RecentQuery {
    n: Option<usize>,
}

async fn log_error(Json(event): Json<susi_error::PostedErrorEvent>) -> StatusCode {
    match susi_error::append_posted_event(event) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn recent_errors(Query(q): Query<RecentQuery>) -> Json<serde_json::Value> {
    let entries = susi_error::recent_error_entries(q.n.unwrap_or(50));
    Json(serde_json::json!({ "returned": entries.len(), "entries": entries }))
}

pub(crate) fn router() -> Router {
    Router::new()
        .route(
            "/errors/recent",
            get(recent_errors).layer(axum::middleware::from_fn(crate::auth::supervisor)),
        )
        .route("/log_error", post(log_error))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

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
        let mut builder = Request::post("/log_error");
        if let Some(value) = content_type {
            builder = builder.header("content-type", value);
        }
        let request = builder.body(Body::from(body.to_string())).unwrap();
        router().oneshot(request).await.unwrap().status()
    }

    // `/log_error` is open by contract, so no middleware precedes the extractor
    // and these rejections are independent of any auth state.
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
        let request = Request::get("/log_error").body(Body::empty()).unwrap();
        let response = router().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn recent_errors_rejects_non_get_methods_and_serves_json_or_unauthorized_on_get() {
        for method in ["POST", "DELETE"] {
            let req = Request::builder()
                .method(method)
                .uri("/errors/recent")
                .body(Body::empty())
                .unwrap();
            let status = router().oneshot(req).await.unwrap().status();
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::UNAUTHORIZED,
                "{method} /errors/recent -> {status}"
            );
        }

        let get_req = Request::get("/errors/recent?n=0")
            .body(Body::empty())
            .unwrap();
        let resp = router().oneshot(get_req).await.unwrap();
        let status = resp.status();
        assert!(
            status == StatusCode::OK || status == StatusCode::UNAUTHORIZED,
            "GET /errors/recent?n=0 -> {status}"
        );
        if status == StatusCode::OK {
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["returned"], 0);
            assert!(body["entries"].is_array());
        }
    }
}
