//! Standalone `susi-native` REST service.
//!
//! Serves Wasmer/WASI reflex execution over HTTP so decoupled crates can run
//! untrusted Wasm without a source dependency on this crate — and without
//! pulling `wasmer`/`wasmer-wasix` into every feature plane. There is no
//! local fallback in vendored clients (same posture as `susi-sandbox`
//! docker exec): Wasm execution requires this service. Default bind:
//! `127.0.0.1:18084` (override with `SUSI_NATIVE_PORT`).

use axum::{http::StatusCode, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use susi_native::wasm::WasmHost;

const DEFAULT_PORT: u16 = 18084;

#[derive(Deserialize)]
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new().route("/wasm/execute", post(wasm_execute));

    let port = std::env::var("SUSI_NATIVE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("susi-native service listening on {addr}");
    axum::serve(listener, app).await
}
