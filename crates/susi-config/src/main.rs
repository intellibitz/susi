//! Standalone `susi-config` REST service.
//!
//! Serves the healed global `SusiConfig` (`~/.susi/config.json`) over HTTP so
//! decoupled crates can read it without a source dependency on this crate —
//! the canonical writer for the shared config file while the substrate is
//! up. Default bind: `127.0.0.1:18082` (override with `SUSI_CONFIG_PORT`).

use axum::{http::StatusCode, routing::get, routing::post, Json, Router};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use susi_config::SusiConfig;

const DEFAULT_PORT: u16 = 18082;

fn config_dir() -> std::path::PathBuf {
    susi_config::susi_paths::SusiDirs::config_dir()
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let app = Router::new()
        .route("/config", get(get_config).post(save_config))
        .route("/config/reload", post(reload_config));

    let port = std::env::var("SUSI_CONFIG_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("susi-config service listening on {addr}");
    axum::serve(listener, app).await
}
