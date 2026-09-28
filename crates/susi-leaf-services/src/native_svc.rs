//! `susi-native`: `POST /wasm/execute` — Wasmer/WASI reflex execution.
//! Supervisor-token gated.

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use std::path::PathBuf;
use susi_vendor_wasmer::wasm::WasmHost;

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
