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

//! # susi-sandbox
//!
//! Leaf REST service for sandbox ensure / Docker exec / daemon integrity
//! helpers. Default bind: `127.0.0.1:18083` (`SUSI_SANDBOX_PORT`). bollard is
//! linked only by this crate; feature crates depend on `susi-sandbox-client`,
//! which also owns the helpers re-exported here. Audit HMAC key ops are never
//! exposed over HTTP.

pub use susi_config;
pub use susi_error;
pub use susi_sandbox_client::{
    audit_chain, daemon_state, extensions, manager, versioned_store, SandboxManager,
    VersionedJsonStore,
};

mod docker;

/// Embedded REST service mode: sandbox ensure/docker-exec and daemon
/// integrity helpers over HTTP. Shared by the standalone `susi-sandbox`
/// binary and the root `susi` binary's `service-run` dispatch.
/// Every route requires the host bearer token: `docker_exec` runs commands
/// and the rest touch the user's substrate; loopback is shared by all local
/// users.
async fn require_bearer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if susi_paths::bearer_authorized(header) {
        next.run(req).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    use crate::daemon_state::local::SusiDaemonState;
    use crate::SandboxManager;
    use axum::{
        extract::Query,
        http::StatusCode,
        routing::{get, post},
        Json, Router,
    };
    use serde::Deserialize;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;

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
        match SandboxManager::ensure_global_sandbox_locally(&req.global_dir) {
            Ok(()) => StatusCode::NO_CONTENT,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    async fn docker_exec(Json(req): Json<DockerExecReq>) -> Result<Json<String>, StatusCode> {
        crate::docker::execute_in_docker(&req.cmd)
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

    susi_sandbox_client::enter_service_mode();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let app = Router::new()
                .route("/sandbox/ensure_global", post(ensure_global))
                .route("/sandbox/docker_exec", post(docker_exec))
                .route("/daemon/status", get(daemon_status))
                .route("/daemon/verify_integrity", post(verify_integrity))
                .route("/daemon/hash_cached", post(hash_cached))
                .layer(axum::middleware::from_fn(require_bearer));
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("susi-sandbox service listening on {addr}");
            axum::serve(listener, app).await
        })
}
