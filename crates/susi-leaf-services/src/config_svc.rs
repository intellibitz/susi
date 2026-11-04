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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn status(method: &str, uri: &str, body: &str) -> StatusCode {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        router().oneshot(request).await.unwrap().status()
    }

    #[test]
    fn config_dir_is_the_non_empty_substrate_config_location() {
        assert!(!config_dir().as_os_str().is_empty());
    }

    // No Authorization header can pass the fail-closed host policy, so these
    // outcomes hold regardless of the substrate root other tests point at.
    #[tokio::test]
    async fn every_config_route_and_method_requires_the_host_token() {
        for (method, uri, body) in [
            ("GET", "/config", ""),
            ("POST", "/config", "{}"),
            ("POST", "/config", "not json"),
            ("POST", "/config/reload", ""),
            ("DELETE", "/config", ""),
            ("GET", "/config/reload", ""),
        ] {
            assert_eq!(
                status(method, uri, body).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_config_paths_do_not_reveal_route_existence_to_anonymous_callers() {
        // The host-token layer also wraps the router fallback, so an anonymous
        // probe gets the same 401 for a real and a non-existent path.
        assert_eq!(
            status("GET", "/config/unknown", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("GET", "/config", "").await,
            status("GET", "/config/unknown", "").await
        );
    }
}
