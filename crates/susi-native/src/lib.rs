#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! # susi-native
//!
//! Leaf REST service for Wasmer/WASI reflex execution. Default bind:
//! `127.0.0.1:18084` (`SUSI_NATIVE_PORT`). Feature crates vendor a
//! byte-identical `susi_native` module and reach this process over a thin
//! HTTP IPC client; there is no local fallback — `wasmer`/`wasmer-wasix`
//! stay linked into the service binary only.

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

pub mod wasm;

/// Embedded REST service mode: Wasmer/WASI reflex execution over HTTP.
/// There is no local fallback in vendored clients — Wasm execution
/// requires this service. Shared by the standalone `susi-native` binary
/// and the root `susi` binary's `service-run` dispatch.
/// Bearer check for a dependency-free leaf service. The daemon's supervisor
/// passes the host token in `SUSI_HOST_TOKEN`; a bare instance started
/// without it (local dev, CI harness) stays open.
fn leaf_authorized(header: Option<&str>) -> bool {
    let Some(expected) = std::env::var("SUSI_HOST_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    else {
        return true;
    };
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let (a, b) = (presented.trim().as_bytes(), expected.trim().as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_bearer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if leaf_authorized(header) {
        next.run(req).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    use crate::wasm::WasmHost;
    use axum::{http::StatusCode, routing::post, Json, Router};
    use serde::Deserialize;
    use serde_json::json;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;

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

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let app = Router::new()
                .route("/wasm/execute", post(wasm_execute))
                .layer(axum::middleware::from_fn(require_bearer));
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("susi-native service listening on {addr}");
            axum::serve(listener, app).await
        })
}
