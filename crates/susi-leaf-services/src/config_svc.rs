//! `susi-config`: the canonical writer of the global `SusiConfig` while the
//! substrate is up. Every route requires the host token.

use std::sync::Arc;

use susi_config::SusiConfig;
use susi_http_transport::server::{
    get, post, IntoResponse, Json, Request, Response, Router, StatusCode,
};

fn config_dir() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir()
}

async fn get_config(_: Request) -> Response {
    match SusiConfig::load_global() {
        Ok(config) => Json(config).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn reload_config(_: Request) -> Response {
    match SusiConfig::reload(&config_dir()) {
        Ok(config) => Json(config).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn save_config(request: Request) -> Response {
    let config = match request.json::<SusiConfig>() {
        Ok(config) => config,
        Err(response) => return response,
    };
    match config.save(&config_dir()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(crate) fn router() -> Router {
    crate::auth::guard_all(
        Router::new()
            .route("/config", get(get_config).post(save_config))
            .route("/config/reload", post(reload_config)),
        Arc::new(crate::auth::host_token),
    )
}

#[cfg(test)]
// Test fixtures are intentionally fail-fast; a malformed request identifies the broken contract.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use susi_http_transport::server::{HeaderMap, HeaderValue};

    fn request(method: &str, uri: &str, body: &str) -> Request {
        let (path, query) = uri.split_once('?').unwrap_or((uri, ""));
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        Request {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            headers,
            body: body.as_bytes().to_vec(),
            peer: None,
        }
    }

    async fn status(method: &str, uri: &str, body: &str) -> StatusCode {
        router().call(request(method, uri, body)).await.status()
    }

    #[test]
    fn config_dir_is_the_non_empty_substrate_config_location() {
        assert!(!config_dir().as_os_str().is_empty());
    }

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
            assert_eq!(status(method, uri, body).await, StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn unknown_config_paths_do_not_reveal_route_existence_to_anonymous_callers() {
        assert_eq!(
            status("GET", "/config/unknown", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("GET", "/config", "").await,
            status("GET", "/config/unknown", "").await
        );
    }

    #[tokio::test]
    async fn config_routes_reject_invalid_bearer_tokens_with_unauthorized() {
        for auth_header in ["Bearer invalid-forged-token", "Basic dXNlcjpwYXNz"] {
            for (method, uri, body) in [
                ("GET", "/config", ""),
                ("POST", "/config", "{}"),
                ("POST", "/config/reload", ""),
            ] {
                let mut request = request(method, uri, body);
                request
                    .headers
                    .insert("authorization", HeaderValue::from_static(auth_header));
                assert_eq!(
                    router().call(request).await.status(),
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} with {auth_header}"
                );
            }
        }
    }

    #[tokio::test]
    async fn put_and_patch_on_config_routes_require_host_token() {
        for (method, uri, body) in [
            ("PUT", "/config", "{}"),
            ("PATCH", "/config", "{}"),
            ("PUT", "/config/reload", ""),
        ] {
            assert_eq!(status(method, uri, body).await, StatusCode::UNAUTHORIZED);
        }
    }
}
