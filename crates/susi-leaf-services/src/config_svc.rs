//! `susi-config`: the canonical writer of the global `SusiConfig` while the
//! substrate is up. Every route requires the host token.

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use susi_config::SusiConfig;

fn config_dir() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir()
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

pub(crate) fn router() -> Router {
    Router::new()
        .route("/config", get(get_config).post(save_config))
        .route("/config/reload", post(reload_config))
        .layer(axum::middleware::from_fn(crate::auth::host_token))
}
