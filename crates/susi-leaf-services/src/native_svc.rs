//! `susi-native`: `POST /wasm/execute` — Wasmtime/WASI reflex execution.
//! Supervisor-token gated.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use susi_http_transport::server::{
    post, IntoResponse, Json, Request, Response, Router, StatusCode,
};
use susi_vendor_wasmtime::wasm::WasmHost;

#[derive(serde::Deserialize)]
struct WasmExecReq {
    wasm_path: PathBuf,
    arg: String,
}

async fn wasm_execute(request: Request) -> Response {
    let request = match request.json::<WasmExecReq>() {
        Ok(request) => request,
        Err(response) => return response,
    };
    // Wasmtime compile + WASI run is blocking; keep it off the async worker.
    let result = tokio::task::spawn_blocking(move || {
        WasmHost::execute_reflex(&request.wasm_path, &request.arg)
    })
    .await;
    match result {
        Ok(Ok(output)) => Json(json!({ "output": output })).into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("wasm task join failed: {error}") })),
        )
            .into_response(),
    }
}

pub(crate) fn router() -> Router {
    Router::new()
        .route("/wasm/execute", post(wasm_execute))
        .guard_all(None, Arc::new(crate::auth::supervisor))
}

#[cfg(test)]
// Test fixtures are intentionally fail-fast; a malformed request identifies the broken contract.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use susi_http_transport::server::HeaderMap;

    fn request(method: &str, uri: &str, body: &str) -> Request {
        let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            susi_http_transport::server::HeaderValue::from_static("application/json"),
        );
        Request {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
            peer: None,
        }
    }

    fn with_token(mut request: Request, token: &'static str) -> Request {
        request.headers.insert(
            "authorization",
            susi_http_transport::server::HeaderValue::from_static(token),
        );
        request
    }

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

    #[tokio::test]
    async fn wasm_execute_is_post_only() {
        let before = std::env::var("SUSI_HOST_TOKEN").ok();
        std::env::remove_var("SUSI_HOST_TOKEN");
        for method in ["GET", "PUT", "DELETE"] {
            let status = router()
                .call(request(method, "/wasm/execute", ""))
                .await
                .status();
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::UNAUTHORIZED,
                "{method}: {status:?}"
            );
        }
        if let Some(token) = before {
            std::env::set_var("SUSI_HOST_TOKEN", token);
        }
    }

    #[tokio::test]
    async fn unknown_native_paths_are_not_routed() {
        let before = std::env::var("SUSI_HOST_TOKEN").ok();
        std::env::remove_var("SUSI_HOST_TOKEN");
        let status = router()
            .call(request("POST", "/wasm/other", ""))
            .await
            .status();
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED);
        if let Some(token) = before {
            std::env::set_var("SUSI_HOST_TOKEN", token);
        }
    }

    #[tokio::test]
    async fn wasm_execute_rejects_malformed_json_or_unauthorized_and_missing_wasm_file_returns_error(
    ) {
        let before = std::env::var("SUSI_HOST_TOKEN").ok();
        std::env::set_var("SUSI_HOST_TOKEN", "native-test-token");
        let malformed_status = router()
            .call(request("POST", "/wasm/execute", "{not json"))
            .await
            .status();
        assert_eq!(malformed_status, StatusCode::UNAUTHORIZED);

        let missing_status = router()
            .call(with_token(
                request(
                    "POST",
                    "/wasm/execute",
                    r#"{"wasm_path":"/nonexistent/susi_missing_reflex_1104.wasm","arg":"--ping"}"#,
                ),
                "Bearer native-test-token",
            ))
            .await
            .status();
        assert_eq!(missing_status, StatusCode::INTERNAL_SERVER_ERROR);
        match before {
            Some(token) => std::env::set_var("SUSI_HOST_TOKEN", token),
            None => std::env::remove_var("SUSI_HOST_TOKEN"),
        }
    }

    #[tokio::test]
    async fn head_and_patch_on_wasm_execute_are_rejected() {
        let before = std::env::var("SUSI_HOST_TOKEN").ok();
        std::env::remove_var("SUSI_HOST_TOKEN");
        for method in ["HEAD", "PATCH"] {
            assert_eq!(
                router()
                    .call(request(method, "/wasm/execute", ""))
                    .await
                    .status(),
                StatusCode::METHOD_NOT_ALLOWED
            );
        }
        if let Some(token) = before {
            std::env::set_var("SUSI_HOST_TOKEN", token);
        }
    }
}
