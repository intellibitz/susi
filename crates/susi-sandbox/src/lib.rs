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
//! helpers. Default bind: `127.0.0.1:18083` (`SUSI_SANDBOX_PORT`). Feature
//! crates vendor a byte-identical `susi_sandbox` module and reach this
//! process over a thin HTTP IPC client (with local filesystem fallback where
//! safe). Audit HMAC key ops are never exposed over HTTP.

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_error.rs"]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_paths.rs"]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../susi-core/src/susi_config.rs"]
pub mod susi_config;

pub mod audit_chain;
pub mod auto_install;
pub mod daemon_state;
pub use crate::susi_config::extensions;
pub mod manager;
pub use crate::susi_config::versioned_store;
pub use crate::susi_config::VersionedJsonStore;
pub use manager::SandboxManager;

/// Serializes tests that mutate or read process-global environment-derived
/// paths (`HOME`, `XDG_CONFIG_HOME`, `SUSI_*`). Mutators must hold this lock
/// for the whole env-swap window; readers of `SusiDirs`-derived paths must
/// hold it while resolving so a swapped HOME cannot flip path selection
/// mid-test.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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
    if crate::susi_paths::bearer_authorized(header) {
        next.run(req).await
    } else {
        axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
    }
}

pub fn serve(port: u16) -> std::io::Result<()> {
    use crate::daemon_state::SusiDaemonState;
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

#[cfg(test)]
mod audit_chain_tests {
    use crate::audit_chain::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_audit() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("susi_audit_chain_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("audit.log")
    }

    #[test]
    fn signed_entries_verify_and_detect_tamper() {
        // The HMAC key path derives from SusiDirs::substrate_home(); serialize
        // against tests that swap HOME so the key does not change mid-test.
        let _guard = crate::env_test_lock();
        let path = temp_audit();
        append_signed_entry(&path, "Info", "TEST_A", "alpha-payload", 1).unwrap();
        append_signed_entry(&path, "Info", "TEST_B", "beta-payload", 1).unwrap();
        assert_eq!(verify_chain(&path).unwrap(), 2);

        let mut content = fs::read_to_string(&path).unwrap();
        content = content.replace("alpha-payload", "EVIL-payload");
        fs::write(&path, &content).unwrap();
        assert!(verify_chain(&path).is_err());
    }
}
