//! Standalone `susi-sandbox` REST service.
//!
//! Serves sandbox ensure/docker-exec and daemon integrity helpers over HTTP so
//! decoupled crates can use them without a source dependency on this crate —
//! and without pulling `bollard` into every feature plane. Audit HMAC key ops
//! stay local-only (same rationale as config's `cluster_key`). Default bind:
//! `127.0.0.1:18083` (override with `SUSI_SANDBOX_PORT`).

use axum::{
    extract::Query,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use susi_sandbox::daemon_state::SusiDaemonState;
use susi_sandbox::SandboxManager;

const DEFAULT_PORT: u16 = 18083;

#[derive(Deserialize)]
struct EnsureGlobalReq {
    global_dir: PathBuf,
}

#[derive(Deserialize)]
struct DockerExecReq {
    cmd: String,
}

#[derive(Deserialize)]
struct DaemonStatusQuery {
    workspace: PathBuf,
    global_dir: PathBuf,
}

#[derive(Deserialize)]
struct BinaryPathsReq {
    bin_path: PathBuf,
    global_dir: PathBuf,
}

async fn ensure_global(Json(req): Json<EnsureGlobalReq>) -> StatusCode {
    match SandboxManager::ensure_global_sandbox(&req.global_dir) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn docker_exec(Json(req): Json<DockerExecReq>) -> Result<Json<String>, StatusCode> {
    SandboxManager::execute_in_docker(&req.cmd)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn daemon_status(Query(q): Query<DaemonStatusQuery>) -> Json<bool> {
    Json(SusiDaemonState::check_status(&q.workspace, &q.global_dir))
}

async fn verify_integrity(Json(req): Json<BinaryPathsReq>) -> Result<Json<bool>, StatusCode> {
    SusiDaemonState::verify_binary_integrity(&req.bin_path, &req.global_dir)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn hash_cached(Json(req): Json<BinaryPathsReq>) -> Result<Json<String>, StatusCode> {
    SusiDaemonState::calculate_binary_hash_cached(&req.bin_path, &req.global_dir)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new()
        .route("/sandbox/ensure_global", post(ensure_global))
        .route("/sandbox/docker_exec", post(docker_exec))
        .route("/daemon/status", get(daemon_status))
        .route("/daemon/verify_integrity", post(verify_integrity))
        .route("/daemon/hash_cached", post(hash_cached));

    let port = std::env::var("SUSI_SANDBOX_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("susi-sandbox service listening on {addr}");
    axum::serve(listener, app).await
}
