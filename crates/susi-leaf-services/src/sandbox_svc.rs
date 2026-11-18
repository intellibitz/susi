//! `susi-sandbox`: sandbox ensure, Docker exec and daemon integrity helpers.
//! Every route requires the host token: `docker_exec` runs commands and the
//! rest touch the user's substrate.

use std::path::PathBuf;
use std::sync::Arc;

use susi_http_transport::server::{
    get, post, IntoResponse, Json, Request, Response, Router, StatusCode,
};
use susi_sandbox_client::daemon_state::local::SusiDaemonState;
use susi_sandbox_client::SandboxManager;

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

async fn ensure_global(request: Request) -> Response {
    let request = match request.json::<EnsureGlobalReq>() {
        Ok(request) => request,
        Err(response) => return response,
    };
    match SandboxManager::ensure_global_sandbox_locally(&request.global_dir) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn docker_exec(request: Request) -> Response {
    let request = match request.json::<DockerExecReq>() {
        Ok(request) => request,
        Err(response) => return response,
    };
    let _ = request.cmd;
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn daemon_status_query(request: &Request) -> Result<DaemonStatusQuery, Response> {
    let mut workspace = None;
    let mut global_dir = None;
    for (name, value) in request.query_pairs() {
        match name.as_str() {
            "workspace" => workspace = Some(PathBuf::from(value)),
            "global_dir" => global_dir = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    match (workspace, global_dir) {
        (Some(workspace), Some(global_dir)) => Ok(DaemonStatusQuery {
            workspace,
            global_dir,
        }),
        (None, _) => Err(Request::query_rejection("workspace: missing field")),
        (_, None) => Err(Request::query_rejection("global_dir: missing field")),
    }
}

async fn daemon_status(request: Request) -> Response {
    let query = match daemon_status_query(&request) {
        Ok(query) => query,
        Err(response) => return response,
    };
    Json(SusiDaemonState::check_status(
        &query.workspace,
        &query.global_dir,
    ))
    .into_response()
}

async fn verify_integrity(request: Request) -> Response {
    let request = match request.json::<BinaryPathsReq>() {
        Ok(request) => request,
        Err(response) => return response,
    };
    SusiDaemonState::verify_binary_integrity(&request.bin_path, &request.global_dir)
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn hash_cached(request: Request) -> Response {
    let request = match request.json::<BinaryPathsReq>() {
        Ok(request) => request,
        Err(response) => return response,
    };
    SusiDaemonState::calculate_binary_hash_cached(&request.bin_path, &request.global_dir)
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(crate) fn router() -> Router {
    Router::new()
        .route("/sandbox/ensure_global", post(ensure_global))
        .route("/sandbox/docker_exec", post(docker_exec))
        .route("/daemon/status", get(daemon_status))
        .route("/daemon/verify_integrity", post(verify_integrity))
        .route("/daemon/hash_cached", post(hash_cached))
        .guard_all(None, Arc::new(crate::auth::host_token))
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

    #[test]
    fn request_types_require_every_field() {
        let ensure: EnsureGlobalReq =
            serde_json::from_value(serde_json::json!({"global_dir": "/g"})).unwrap();
        assert_eq!(ensure.global_dir, PathBuf::from("/g"));
        assert!(serde_json::from_value::<EnsureGlobalReq>(serde_json::json!({})).is_err());

        let exec: DockerExecReq =
            serde_json::from_value(serde_json::json!({"cmd": "echo hi"})).unwrap();
        assert_eq!(exec.cmd, "echo hi");
        assert!(serde_json::from_value::<DockerExecReq>(serde_json::json!({})).is_err());
        assert!(serde_json::from_value::<DockerExecReq>(serde_json::json!({"cmd": 1})).is_err());

        let status: DaemonStatusQuery = serde_json::from_value(serde_json::json!({
            "workspace": "/w",
            "global_dir": "/g"
        }))
        .unwrap();
        assert_eq!(status.workspace, PathBuf::from("/w"));
        assert_eq!(status.global_dir, PathBuf::from("/g"));
        assert!(serde_json::from_value::<DaemonStatusQuery>(
            serde_json::json!({"workspace": "/w"})
        )
        .is_err());
        assert!(serde_json::from_value::<DaemonStatusQuery>(
            serde_json::json!({"global_dir": "/g"})
        )
        .is_err());

        let paths: BinaryPathsReq = serde_json::from_value(serde_json::json!({
            "bin_path": "/b",
            "global_dir": "/g"
        }))
        .unwrap();
        assert_eq!(paths.bin_path, PathBuf::from("/b"));
        assert_eq!(paths.global_dir, PathBuf::from("/g"));
        assert!(
            serde_json::from_value::<BinaryPathsReq>(serde_json::json!({"bin_path": "/b"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<BinaryPathsReq>(serde_json::json!({"global_dir": "/g"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn every_sandbox_route_requires_the_host_token() {
        let _env = crate::ENV_LOCK.lock().await;
        let before = std::env::var("SUSI_HOME").ok();
        let home = susi_paths::tempfile::tempdir().unwrap();
        std::env::set_var("SUSI_HOME", home.path());
        for (method, uri, body) in [
            ("POST", "/sandbox/ensure_global", r#"{"global_dir":"/g"}"#),
            ("POST", "/sandbox/docker_exec", r#"{"cmd":"echo hi"}"#),
            ("GET", "/daemon/status?workspace=/w&global_dir=/g", ""),
            ("GET", "/daemon/status", ""),
            (
                "POST",
                "/daemon/verify_integrity",
                r#"{"bin_path":"/b","global_dir":"/g"}"#,
            ),
            (
                "POST",
                "/daemon/hash_cached",
                r#"{"bin_path":"/b","global_dir":"/g"}"#,
            ),
            ("POST", "/daemon/hash_cached", "not json"),
            ("GET", "/sandbox/docker_exec", ""),
        ] {
            let status = router().call(request(method, uri, body)).await.status();
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
        match before {
            Some(value) => std::env::set_var("SUSI_HOME", value),
            None => std::env::remove_var("SUSI_HOME"),
        }
    }

    #[tokio::test]
    async fn unsupported_methods_and_unknown_sandbox_paths_fail_closed_without_host_token() {
        for (method, uri) in [
            ("GET", "/sandbox/ensure_global"),
            ("DELETE", "/daemon/status"),
            ("PUT", "/daemon/verify_integrity"),
            ("GET", "/daemon/hash_cached"),
            ("GET", "/sandbox/unknown"),
            ("POST", "/daemon/unknown"),
        ] {
            let status = router().call(request(method, uri, "")).await.status();
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn sandbox_routes_reject_forged_bearer_tokens_with_unauthorized() {
        for auth_header in ["Bearer forged-invalid-token-1104", "Basic dXNlcjpwYXNz"] {
            for (method, uri, body) in [
                ("POST", "/sandbox/ensure_global", r#"{"global_dir":"/g"}"#),
                ("POST", "/sandbox/docker_exec", r#"{"cmd":"echo hi"}"#),
                ("GET", "/daemon/status?workspace=/w&global_dir=/g", ""),
                (
                    "POST",
                    "/daemon/verify_integrity",
                    r#"{"bin_path":"/b","global_dir":"/g"}"#,
                ),
                (
                    "POST",
                    "/daemon/hash_cached",
                    r#"{"bin_path":"/b","global_dir":"/g"}"#,
                ),
            ] {
                let mut request = request(method, uri, body);
                request
                    .headers
                    .insert("authorization", HeaderValue::from_static(auth_header));
                assert_eq!(
                    router().call(request).await.status(),
                    StatusCode::UNAUTHORIZED
                );
            }
        }
    }

    #[tokio::test]
    async fn patch_and_head_on_sandbox_routes_fail_closed_without_host_token() {
        for (method, uri) in [
            ("PATCH", "/sandbox/docker_exec"),
            ("HEAD", "/daemon/status"),
            ("DELETE", "/sandbox/ensure_global"),
        ] {
            assert_eq!(
                router().call(request(method, uri, "")).await.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }
}
