#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Shared HTTP transport for every SUSI host-contract surface (GEMI REST,
//! GMCP, A2A): a TLS-sniffing accept helper, a Hyper connection builder
//! with a header-read bound, the std HTTP/1.1 server ([`server`]) that the
//! harness, the A2A port and the leaf services share, and the outbound
//! `http_call` client: HTTP/1.1 on std sockets with verified platform TLS.
//! Vendor SDKs stay out.

pub mod client;
mod connect;
pub mod dual_transport;
pub mod http_body;
pub mod http_conn;
pub mod server;
mod wire;

pub use client::{http_call, http_call_with_body, http_post_utf8, HttpCall};

#[cfg(test)]
mod surface_tests {
    use super::*;

    /// A loopback URL nothing is listening on: bind an ephemeral port, then
    /// release it before the call.
    fn refused_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        format!("http://127.0.0.1:{port}/probe")
    }

    #[test]
    fn root_re_exports_are_the_client_items() {
        assert_eq!(
            std::any::TypeId::of::<HttpCall>(),
            std::any::TypeId::of::<client::HttpCall>()
        );
        // The root function names are exercised by the transport tests below.
    }

    #[test]
    fn transport_failures_are_errors_not_statuses() {
        let url = refused_url();
        assert!(http_call("GET", &url, &[], 2, 0).is_err());
        assert!(http_call_with_body("POST", &url, &[], Some(b"{}"), 2, 0).is_err());
        assert!(http_post_utf8(&url, &[], b"{}", 2, 1024).is_err());
        assert!(http_call("GET", "not a url", &[], 2, 0).is_err());
    }

    #[test]
    fn unsupported_methods_are_refused_before_any_request() {
        let err = http_call_with_body("TRACE", &refused_url(), &[], None, 2, 0)
            .err()
            .expect("TRACE must be refused");
        assert_eq!(err, "unsupported HTTP method TRACE");
    }

    #[test]
    fn empty_or_lowercase_unsupported_methods_are_refused_before_any_socket_io() {
        let url = refused_url();
        let empty_err = http_call("", &url, &[], 1, 0)
            .err()
            .expect("empty method must be refused");
        assert!(empty_err.contains("unsupported HTTP method"));

        for method in ["get", "OPTIONS", "CONNECT"] {
            let err = http_call_with_body(method, &url, &[], None, 1, 0)
                .err()
                .expect("method must be refused");
            assert_eq!(err, format!("unsupported HTTP method {method}"));
        }
    }

    #[test]
    fn refused_url_produces_http_loopback_probe_endpoint_and_put_delete_fail_on_refused_port() {
        let url = refused_url();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert!(url.ends_with("/probe"));
        assert!(http_call_with_body("PUT", &url, &[], Some(b"{}"), 1, 0).is_err());
        assert!(http_call("DELETE", &url, &[], 1, 0).is_err());
    }
}
