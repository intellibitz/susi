//! `susi-sandbox`: sandbox ensure, Docker exec and daemon integrity helpers.
//! Every route requires the host token: `docker_exec` runs commands and the
//! rest touch the user's substrate.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use std::path::PathBuf;
use susi_sandbox::daemon_state::local::SusiDaemonState;
use susi_sandbox::SandboxManager;

#[derive(serde::Deserialize)]
struct EnsureGlobalReq {
    global_dir: PathBuf,
}

#[derive(serde::Deserialize)]
struct DockerExecReq {
    cmd: String,
}

#[derive(serde::Deserialize)]
struct DaemonStatusQuery {
    workspace: PathBuf,
    global_dir: PathBuf,
}

#[derive(serde::Deserialize)]
struct BinaryPathsReq {
    bin_path: PathBuf,
    global_dir: PathBuf,
}

async fn ensure_global(Json(req): Json<EnsureGlobalReq>) -> StatusCode {
    match SandboxManager::ensure_global_sandbox_locally(&req.global_dir) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn docker_exec(Json(req): Json<DockerExecReq>) -> Result<Json<String>, StatusCode> {
    susi_sandbox::execute_in_docker(&req.cmd)
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

pub(crate) fn router() -> Router {
    Router::new()
        .route("/sandbox/ensure_global", post(ensure_global))
        .route("/sandbox/docker_exec", post(docker_exec))
        .route("/daemon/status", get(daemon_status))
        .route("/daemon/verify_integrity", post(verify_integrity))
        .route("/daemon/hash_cached", post(hash_cached))
        .layer(axum::middleware::from_fn(crate::auth::host_token))
}
