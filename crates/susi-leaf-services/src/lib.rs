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
}
