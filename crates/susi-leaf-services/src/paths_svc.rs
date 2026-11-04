//! `susi-paths`: `GET /paths`, `GET /ports` — the host path/port contract.

use axum::{routing::get, Json, Router};

pub(crate) fn router() -> Router {
    Router::new()
        .route(
            "/paths",
            get(|| async { Json(susi_paths::local_paths_json()) }),
        )
        .route("/ports", get(|| async { Json(susi_paths::ports_json()) }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn call(method: &str, uri: &str) -> axum::response::Response {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        router().oneshot(request).await.unwrap()
    }

    async fn json_body(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn paths_and_ports_answer_json_objects() {
        for uri in ["/paths", "/ports"] {
            let response = call("GET", uri).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let content_type = response.headers().get("content-type").unwrap().clone();
            assert!(content_type
                .to_str()
                .unwrap()
                .starts_with("application/json"));
            let body = json_body(response).await;
            let object = body.as_object().unwrap_or_else(|| panic!("{uri}: {body}"));
            assert!(!object.is_empty(), "{uri} must not be an empty contract");
        }
    }

    #[tokio::test]
    async fn contract_routes_are_read_only_and_closed_to_unknown_paths() {
        for uri in ["/paths", "/ports"] {
            for method in ["POST", "PUT", "DELETE"] {
                assert_eq!(
                    call(method, uri).await.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {uri}"
                );
            }
        }
        assert_eq!(call("GET", "/other").await.status(), StatusCode::NOT_FOUND);
    }
}
