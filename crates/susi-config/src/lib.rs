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
//! substrate is up. Every other crate depends on this crate; only the
//! *global* config read/write path prefers the service, so a running
//! substrate stays the canonical writer. When the service is unreachable —
//! or explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) is set, e.g. in
//! tests with a swapped `HOME` — every call resolves against the local files,
//! so config access never hard-fails on service health. `cluster_key`
//! signing/verification is deliberately local only: exposing HMAC over
//! `cluster.key` (0600) as an unauthenticated localhost endpoint would let
//! any process mint signed cluster messages.

pub use susi_error;

pub mod cloud_env;
pub mod cluster_key;
mod config;
pub mod extensions;
pub mod file_lock;
mod json_util;
mod types;
pub mod versioned_store;

/// Set by [`serve`]: the service is the canonical writer of the global
/// config file and must never route through an IPC hop back to itself.
static SERVICE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// IPC client for the standalone `susi-config` service. Only the global
/// config path is routed here; per-directory loads and `cluster_key` stay
/// local by construction.
mod service {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::SusiConfig;

    const DEFAULT_PORT: u16 = 18082;
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn addr() -> SocketAddr {
        let port = std::env::var("SUSI_CONFIG_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// The service process, or explicit local env config (`SUSI_XDG`,
    /// `XDG_*_HOME`), resolves locally — a swapped HOME in tests must not
    /// read or write the host substrate's real `config.json`.
    fn local_only() -> bool {
        super::SERVICE_MODE.load(Ordering::Relaxed)
            || [
                "SUSI_XDG",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_CACHE_HOME",
            ]
            .iter()
            .any(|v| std::env::var_os(v).is_some())
    }

    /// Round-trips one HTTP/1.0 request; `None` on any transport failure.
    fn request(req: &str) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&addr(), TIMEOUT).ok()?;
        let _ = stream.set_read_timeout(Some(TIMEOUT));
        let _ = stream.set_write_timeout(Some(TIMEOUT));
        let req = susi_paths::with_bearer(req);
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }

    /// Response body when the status line is a 2xx, else `None`.
    fn body(response: &str) -> Option<&str> {
        let status_ok = response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2");
        if !status_ok {
            return None;
        }
        response.split("\r\n\r\n").nth(1)
    }

    /// `GET /config` — healed global config from the running substrate.
    pub fn get_global() -> Option<SusiConfig> {
        if local_only() {
            return None;
        }
        let resp = request("GET /config HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `POST /config` — route a global-dir save through the substrate when
    /// it is up; `false` tells the caller to fall back to the local atomic
    /// write (identical bytes, same file).
    pub fn save_global(cfg: &SusiConfig) -> bool {
        if local_only() {
            return false;
        }
        let Ok(payload) = serde_json::to_string(cfg) else {
            return false;
        };
        let req = format!(
            "POST /config HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        request(&req)
            .is_some_and(|resp| resp.starts_with("HTTP/1.1 2") || resp.starts_with("HTTP/1.0 2"))
    }
}

pub use cloud_env::{cloud_env_overlay, env_or_cloud_env};
pub use config::{redact_credentials, SusiConfig};
pub use json_util::{
    atomic_replace_file, atomic_write_bytes, atomic_write_json_pretty, confined_workspace_join,
    create_private_dir, http_agent, install_private_file, load_or_create_secret,
    merge_missing_json_defaults, merge_missing_registry_defaults, remove_file_if_present,
    DynamicRegistry, DynamicValue, ModelTier, ProviderType, StringRegistry,
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
    SERVICE_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
    use axum::{http::StatusCode, routing::get, routing::post, Json, Router};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn config_dir() -> std::path::PathBuf {
        susi_paths::SusiDirs::config_dir()
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
        if susi_paths::bearer_authorized(header) {
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
