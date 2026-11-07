//! `susi-sandbox`: sandbox ensure, Docker exec and daemon integrity helpers.
//! Every route requires the host token: `docker_exec` runs commands and the
//! rest touch the user's substrate.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use std::path::PathBuf;
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

async fn ensure_global(Json(req): Json<EnsureGlobalReq>) -> StatusCode {
    match SandboxManager::ensure_global_sandbox_locally(&req.global_dir) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn docker_exec(Json(req): Json<DockerExecReq>) -> Result<Json<String>, StatusCode> {
    let _ = &req.cmd;
    Err(StatusCode::INTERNAL_SERVER_ERROR)
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test-only module: unwrap/expect/panic are how these tests assert.
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::json;
    use tower::ServiceExt;

    #[test]
    fn request_types_require_every_field() {
        let ensure: EnsureGlobalReq = serde_json::from_value(json!({"global_dir": "/g"})).unwrap();
        assert_eq!(ensure.global_dir, PathBuf::from("/g"));
        assert!(serde_json::from_value::<EnsureGlobalReq>(json!({})).is_err());

        let exec: DockerExecReq = serde_json::from_value(json!({"cmd": "echo hi"})).unwrap();
        assert_eq!(exec.cmd, "echo hi");
        assert!(serde_json::from_value::<DockerExecReq>(json!({})).is_err());
        assert!(serde_json::from_value::<DockerExecReq>(json!({"cmd": 1})).is_err());

        let status: DaemonStatusQuery =
            serde_json::from_value(json!({"workspace": "/w", "global_dir": "/g"})).unwrap();
        assert_eq!(status.workspace, PathBuf::from("/w"));
        assert_eq!(status.global_dir, PathBuf::from("/g"));
        assert!(serde_json::from_value::<DaemonStatusQuery>(json!({"workspace": "/w"})).is_err());
        assert!(serde_json::from_value::<DaemonStatusQuery>(json!({"global_dir": "/g"})).is_err());

        let paths: BinaryPathsReq =
            serde_json::from_value(json!({"bin_path": "/b", "global_dir": "/g"})).unwrap();
        assert_eq!(paths.bin_path, PathBuf::from("/b"));
        assert_eq!(paths.global_dir, PathBuf::from("/g"));
        assert!(serde_json::from_value::<BinaryPathsReq>(json!({"bin_path": "/b"})).is_err());
        assert!(serde_json::from_value::<BinaryPathsReq>(json!({"global_dir": "/g"})).is_err());
    }

    // The host policy fails closed and a header-less request can never pass it,
    // so each route (including the command-executing one) answers 401 before
    // any extractor or handler runs, whatever the process env holds.
    #[tokio::test]
    async fn every_sandbox_route_requires_the_host_token() {
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
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let status = router().oneshot(request).await.unwrap().status();
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
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
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let status = router().oneshot(request).await.unwrap().status();
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
                let request = Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("authorization", auth_header)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap();
                let status = router().oneshot(request).await.unwrap().status();
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "{method} {uri} ({auth_header})"
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
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let status = router().oneshot(request).await.unwrap().status();
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }
}
