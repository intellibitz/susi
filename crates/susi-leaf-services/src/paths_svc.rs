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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
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
        let _env = crate::ENV_LOCK.lock().await;
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
        let _env = crate::ENV_LOCK.lock().await;
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

    #[tokio::test]
    async fn paths_and_ports_match_susi_paths_contract_keys_and_values() {
        let _env = crate::ENV_LOCK.lock().await;
        let paths_resp = json_body(call("GET", "/paths").await).await;
        let expected_paths = susi_paths::local_paths_json();
        let paths_obj = paths_resp.as_object().unwrap();
        let expected_paths_obj = expected_paths.as_object().unwrap();
        assert_eq!(paths_obj.len(), expected_paths_obj.len());
        for key in expected_paths_obj.keys() {
            let val = paths_obj
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            assert!(!val.is_empty(), "/paths key {key} must be non-empty string");
        }

        let ports_resp = json_body(call("GET", "/ports").await).await;
        let expected_ports = susi_paths::ports_json();
        let ports_obj = ports_resp.as_object().unwrap();
        let expected_ports_obj = expected_ports.as_object().unwrap();
        assert_eq!(ports_obj, expected_ports_obj);
    }

    #[tokio::test]
    async fn paths_and_ports_ignore_query_strings_and_reject_patch_mutations() {
        let _env = crate::ENV_LOCK.lock().await;
        let paths_resp = call("GET", "/paths?format=compact").await;
        assert_eq!(paths_resp.status(), StatusCode::OK);
        assert_eq!(json_body(paths_resp).await, susi_paths::local_paths_json());

        let ports_resp = call("GET", "/ports?unused=1").await;
        assert_eq!(ports_resp.status(), StatusCode::OK);
        assert_eq!(json_body(ports_resp).await, susi_paths::ports_json());

        assert_eq!(
            call("PATCH", "/paths").await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            call("PATCH", "/ports").await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn head_requests_on_paths_and_ports_return_ok_with_empty_body() {
        let _env = crate::ENV_LOCK.lock().await;
        for uri in ["/paths", "/ports"] {
            let resp = call("HEAD", uri).await;
            assert_eq!(resp.status(), StatusCode::OK, "HEAD {uri}");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(bytes.is_empty(), "HEAD {uri} body must be empty");
        }
    }
}
