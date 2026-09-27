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
//! GMCP, A2A): a TLS-sniffing accept helper and a Hyper connection builder
//! with a header-read bound.

pub mod dual_transport;
pub mod http_conn;
