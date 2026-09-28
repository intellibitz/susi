#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! HTTP shells for the microkernel's leaf services.
//!
//! `susi-paths`, `susi-error` and `susi-config` are foundation libraries
//! every crate links, and `susi-sandbox` (bollard) / `susi-vendor-wasmer` (wasmer)
//! are vendor integrations; none of them carries an HTTP framework. This
//! crate is the one place the leaf services meet axum: one runtime/bind helper, one pair of
//! bearer policies, one `service-run` dispatch. Standalone service bins enable
//! only their own feature so Wasmer and Bollard stay out of unrelated binaries.
//! The service logic stays in each library as plain functions, so a shell is only routing.

#[cfg(any(
    feature = "service-paths",
    feature = "service-error",
    feature = "service-config",
    feature = "service-sandbox",
    feature = "service-native"
))]
mod auth;
#[cfg(feature = "service-config")]
mod config_svc;
#[cfg(feature = "service-error")]
mod error_svc;
#[cfg(feature = "service-native")]
mod native_svc;
#[cfg(feature = "service-paths")]
mod paths_svc;
#[cfg(feature = "service-sandbox")]
mod sandbox_svc;

#[cfg(any(
    feature = "service-paths",
    feature = "service-error",
    feature = "service-config",
    feature = "service-sandbox",
    feature = "service-native"
))]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Serve leaf service `name` on `port` (blocks). Dispatched by the root
/// binary's `service-run` mode and by the standalone service binaries.
///
/// # Errors
/// Unknown service name, or the listener cannot bind/serve.
pub fn serve(name: &str, port: u16) -> std::io::Result<()> {
    // Zero-feature builds still parse the dispatch table; `port` is used by
    // every feature-gated arm.
    let _ = port;
    match name {
        #[cfg(feature = "service-paths")]
        "susi-paths" => run(name, port, paths_svc::router()),
        #[cfg(feature = "service-error")]
        "susi-error" => {
            susi_error::enter_service_mode();
            run(name, port, error_svc::router())
        }
        #[cfg(feature = "service-config")]
        "susi-config" => {
            susi_config::enter_service_mode();
            run(name, port, config_svc::router())
        }
        #[cfg(feature = "service-sandbox")]
        "susi-sandbox" => {
            susi_sandbox_client::enter_service_mode();
            run(name, port, sandbox_svc::router())
        }
        #[cfg(feature = "service-native")]
        "susi-native" => run(name, port, native_svc::router()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("leaf service '{name}' has no embedded serve entry"),
        )),
    }
}

/// Standalone-binary entry: resolve the port from the service table (env
/// override, else default + instance offset from env or config) and serve.
///
/// # Errors
/// See [`serve`].
pub fn run_standalone(name: &str) -> std::io::Result<()> {
    let port = susi_core::service_table::leaf_service(name)
        .map(susi_core::service_table::LeafService::port)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("'{name}' is not in LEAF_SERVICES"),
            )
        })?;
    serve(name, port)
}

/// One multi-threaded runtime, loopback bind, serve `app` until shutdown.
/// Every leaf service also answers an unauthenticated `GET /healthz`
/// (`{"service": name}`) so supervisors and CI can probe liveness without
/// the host token.
#[cfg(any(
    feature = "service-paths",
    feature = "service-error",
    feature = "service-config",
    feature = "service-sandbox",
    feature = "service-native"
))]
fn run(name: &str, port: u16, app: axum::Router) -> std::io::Result<()> {
    let service = name.to_string();
    let app = app.route(
        "/healthz",
        axum::routing::get(move || {
            let service = service.clone();
            async move { axum::Json(serde_json::json!({ "service": service, "status": "ok" })) }
        }),
    );
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("{name} service listening on {addr}");
            axum::serve(listener, app).await
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_service_name_rejected() {
        let err = super::serve("definitely-not-a-service", 0).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("definitely-not-a-service"));
    }

    #[test]
    fn run_standalone_rejects_names_outside_the_leaf_table() {
        let err = super::run_standalone("definitely-not-a-service").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}

/// Router-level tests drive the feature-gated service shells through real
/// HTTP requests (`tower::ServiceExt::oneshot`) — no listener needed.
/// `cargo test -p susi-leaf-services --all-features` exercises every router;
/// a default-feature build compiles this module empty.
#[cfg(all(
    test,
    feature = "service-paths",
    feature = "service-error",
    feature = "service-config",
    feature = "service-sandbox",
    feature = "service-native"
))]
mod router_tests {
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    fn post_json(uri: &str, json: &str) -> Request<Body> {
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap()
    }

    fn bearer(req: Request<Body>, token: &str) -> Request<Body> {
        let (mut parts, body) = req.into_parts();
        parts
            .headers
            .insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        Request::from_parts(parts, body)
    }

    /// `SUSI_HOME` gives the test a fully isolated substrate root; seeding
    /// `<root>/api_token` arms the host-token policy.
    fn isolated_home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("SUSI_HOME", dir.path());
        dir
    }

    fn seed_host_token(home: &std::path::Path, token: &str) {
        std::fs::write(home.join("api_token"), token).unwrap();
    }

    async fn status(app: axum::Router, req: Request<Body>) -> StatusCode {
        app.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn paths_router_answers_the_contract_without_auth() {
        let _g = ENV_LOCK.lock().unwrap();
        let _home = isolated_home();
        assert_eq!(
            status(super::paths_svc::router(), get("/paths")).await,
            StatusCode::OK
        );
        assert_eq!(
            status(super::paths_svc::router(), get("/ports")).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn config_router_fails_closed_until_the_host_token_matches() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = isolated_home();
        // No api_token seeded: the write/read policy must fail closed.
        assert_eq!(
            status(super::config_svc::router(), get("/config")).await,
            StatusCode::UNAUTHORIZED
        );
        seed_host_token(home.path(), "test-host-token");
        assert_eq!(
            status(super::config_svc::router(), get("/config")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                super::config_svc::router(),
                bearer(get("/config"), "test-host-token")
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn error_router_logs_events_and_gates_history() {
        let _g = ENV_LOCK.lock().unwrap();
        let _home = isolated_home();
        // SUSI_HOST_TOKEN unset: supervisor policy is open by contract.
        std::env::remove_var("SUSI_HOST_TOKEN");
        let logged = status(
            super::error_svc::router(),
            post_json(
                "/log_error",
                r#"{"message":"router test","crate":"susi-leaf-services"}"#,
            ),
        )
        .await;
        assert_eq!(logged, StatusCode::NO_CONTENT);
        assert_eq!(
            status(super::error_svc::router(), get("/errors/recent?n=5")).await,
            StatusCode::OK
        );
        // With the supervisor token armed, a wrong bearer is rejected.
        std::env::set_var("SUSI_HOST_TOKEN", "sup-token");
        assert_eq!(
            status(
                super::error_svc::router(),
                bearer(get("/errors/recent"), "wrong")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                super::error_svc::router(),
                bearer(get("/errors/recent"), "sup-token")
            )
            .await,
            StatusCode::OK
        );
        std::env::remove_var("SUSI_HOST_TOKEN");
    }

    #[tokio::test]
    async fn sandbox_router_gates_everything_on_the_host_token() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = isolated_home();
        assert_eq!(
            status(
                super::sandbox_svc::router(),
                post_json("/sandbox/docker_exec", r#"{"cmd":"echo hi"}"#)
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        seed_host_token(home.path(), "test-host-token");
        // Without a docker daemon the exec surfaces a typed 500, not a panic.
        std::env::set_var("DOCKER_HOST", "unix:///nonexistent-susi-test.sock");
        assert_eq!(
            status(
                super::sandbox_svc::router(),
                bearer(
                    post_json("/sandbox/docker_exec", r#"{"cmd":"echo hi"}"#),
                    "test-host-token"
                )
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        // Daemon status over a temp workspace answers a typed bool.
        assert_eq!(
            status(
                super::sandbox_svc::router(),
                bearer(
                    get(&format!(
                        "/daemon/status?workspace={}&global_dir={}",
                        home.path().join("ws").display(),
                        home.path().display()
                    )),
                    "test-host-token"
                )
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn native_router_executes_reflexes_and_surfaces_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let _home = isolated_home();
        std::env::set_var("SUSI_HOST_TOKEN", "sup-token");
        assert_eq!(
            status(
                super::native_svc::router(),
                post_json("/wasm/execute", "{}")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        let res = super::native_svc::router()
            .oneshot(bearer(
                post_json(
                    "/wasm/execute",
                    r#"{"wasm_path":"/nonexistent/reflex.wasm","arg":""}"#,
                ),
                "sup-token",
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        std::env::remove_var("SUSI_HOST_TOKEN");
    }
}
