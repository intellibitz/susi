#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! SUSI configuration substrate: `SusiConfig` dynamic registry, typed config
//! fragments, and shared self-healing JSON load/merge/save helpers.
//!
//! Runs as a standalone REST service (`127.0.0.1:18082`, see `main.rs`) — the
//! canonical reader/writer for the shared `~/.susi/config.json` while the
//! substrate is up. Consumer crates vendor the byte-identical `susi_config`
//! module (surface + IPC client) instead of depending on this crate;
//! `susi-sandbox` re-exports its vendored copy through `susi_sandbox::manager`
//! for back-compat. This crate reaches the foundational `susi-error`/
//! `susi-paths` services through the vendored IPC-client modules below,
//! never on feature crates above it.

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_error.rs"]
pub mod susi_error;
// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
// `pub` so the standalone service binary (main.rs) can resolve the
// global config dir through the same contract as every consumer.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_paths.rs"]
pub mod susi_paths;

// The shared modules below are also `#[path]`-mounted by every other crate
// (via `crates/susi-core/src/susi_config.rs`), so they name siblings with
// `super::` and reach the service through `super::service`.
pub mod cluster_key;
mod config;
pub mod extensions;
mod json_util;
mod types;
pub mod versioned_store;

/// Service hook seen by the shared `config` module. This process *is* the
/// `susi-config` service — the canonical writer of the global config file —
/// so the global path always resolves against the local files, never through
/// an IPC hop back to itself.
mod service {
    pub fn get_global() -> Option<super::SusiConfig> {
        None
    }
    pub fn save_global(_cfg: &super::SusiConfig) -> bool {
        false
    }
}

pub use config::SusiConfig;
pub use json_util::{
    atomic_write_json_pretty, confined_workspace_join, http_agent, merge_missing_json_defaults,
    merge_missing_registry_defaults, DynamicRegistry, DynamicValue, ModelTier, ProviderType,
    StringRegistry,
};
pub use types::*;
pub use versioned_store::VersionedJsonStore;

#[cfg(test)]
#[path = "tests/cluster_key.rs"]
mod cluster_key_tests;
#[cfg(test)]
#[path = "tests/extensions.rs"]
mod extensions_tests;
#[cfg(test)]
#[path = "tests/json_util.rs"]
mod json_util_tests;
#[cfg(test)]
#[path = "tests/versioned_store.rs"]
mod versioned_store_tests;

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

/// Embedded REST service mode: serves the healed global `SusiConfig` over
/// HTTP — the canonical writer for the shared config file while the
/// substrate is up. Shared by the standalone `susi-config` binary and the
/// root `susi` binary's `service-run` dispatch.
pub fn serve(port: u16) -> std::io::Result<()> {
    use axum::{http::StatusCode, routing::get, routing::post, Json, Router};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn config_dir() -> std::path::PathBuf {
        crate::susi_paths::SusiDirs::config_dir()
    }

    /// Every route requires the host bearer token: these endpoints read or
    /// write the user's substrate, and loopback is shared by all local users.
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

    async fn get_config() -> Result<Json<SusiConfig>, StatusCode> {
        SusiConfig::load_global()
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    async fn reload_config() -> Result<Json<SusiConfig>, StatusCode> {
        SusiConfig::reload(&config_dir())
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }

    async fn save_config(Json(cfg): Json<SusiConfig>) -> StatusCode {
        match cfg.save(&config_dir()) {
            Ok(()) => StatusCode::NO_CONTENT,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let app = Router::new()
                .route("/config", get(get_config).post(save_config))
                .route("/config/reload", post(reload_config))
                .layer(axum::middleware::from_fn(require_bearer));
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("susi-config service listening on {addr}");
            axum::serve(listener, app).await
        })
}
