//! `susi-native`: `POST /wasm/execute` — Wasmer/WASI reflex execution.
//! Supervisor-token gated.

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use std::path::PathBuf;
use susi_vendor_wasmtime::wasm::WasmHost;

#[derive(serde::Deserialize)]
struct WasmExecReq {
    wasm_path: PathBuf,
    arg: String,
}

async fn wasm_execute(Json(req): Json<WasmExecReq>) -> (StatusCode, Json<serde_json::Value>) {
    // Wasmer compile + WASI run is blocking; keep it off the async worker.
    let result =
        tokio::task::spawn_blocking(move || WasmHost::execute_reflex(&req.wasm_path, &req.arg))
            .await;
    match result {
        Ok(Ok(output)) => (StatusCode::OK, Json(json!({ "output": output }))),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("wasm task join failed: {e}") })),
        ),
    }
}

pub(crate) fn router() -> Router {
    Router::new()
        .route("/wasm/execute", post(wasm_execute))
        .layer(axum::middleware::from_fn(crate::auth::supervisor))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn wasm_exec_req_requires_both_fields() {
        let ok: WasmExecReq = serde_json::from_value(json!({
            "wasm_path": "/tmp/a.wasm",
            "arg": "--help"
        }))
        .unwrap();
        assert_eq!(ok.wasm_path, PathBuf::from("/tmp/a.wasm"));
        assert_eq!(ok.arg, "--help");
        assert!(
            serde_json::from_value::<WasmExecReq>(json!({"wasm_path": "/tmp/a.wasm"})).is_err()
        );
        assert!(serde_json::from_value::<WasmExecReq>(json!({"arg": "--help"})).is_err());
        assert!(
            serde_json::from_value::<WasmExecReq>(json!({"wasm_path": 7, "arg": "x"})).is_err()
        );
    }

    // The supervisor policy is open without SUSI_HOST_TOKEN and 401 with it, so
    // a method mismatch is either 405 or 401 depending on the process env, and
    // never a successful execution.
    #[tokio::test]
    async fn wasm_execute_is_post_only() {
        for method in ["GET", "PUT", "DELETE"] {
            let request = Request::builder()
                .method(method)
                .uri("/wasm/execute")
                .body(Body::empty())
                .unwrap();
            let status = router().oneshot(request).await.unwrap().status();
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::UNAUTHORIZED,
                "{method} -> {status}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_native_paths_are_not_routed() {
        let request = Request::post("/wasm/other").body(Body::empty()).unwrap();
        let response = router().oneshot(request).await.unwrap();
        // The supervisor layer wraps the fallback too: 404 when open, 401 when
        // the process env arms SUSI_HOST_TOKEN. Never a routed success.
        let status = response.status();
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED,
            "{status}"
        );
    }

    #[tokio::test]
    async fn wasm_execute_rejects_malformed_json_or_unauthorized_and_missing_wasm_file_returns_error(
    ) {
        let malformed_req = Request::post("/wasm/execute")
            .header("content-type", "application/json")
            .body(Body::from("{not json"))
            .unwrap();
        let malformed_status = router().oneshot(malformed_req).await.unwrap().status();
        assert!(
            malformed_status == StatusCode::BAD_REQUEST
                || malformed_status == StatusCode::UNAUTHORIZED,
            "malformed /wasm/execute -> {malformed_status}"
        );

        let missing_req = Request::post("/wasm/execute")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"wasm_path":"/nonexistent/susi_missing_reflex_1104.wasm","arg":"--ping"}"#,
            ))
            .unwrap();
        let missing_status = router().oneshot(missing_req).await.unwrap().status();
        assert!(
            missing_status == StatusCode::INTERNAL_SERVER_ERROR
                || missing_status == StatusCode::UNAUTHORIZED,
            "missing wasm /wasm/execute -> {missing_status}"
        );
    }
}
