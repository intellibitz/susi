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
//! with a header-read bound, and outbound `http_call` over crate-private
//! `ureq`. Callers never name the vendor type. Vendor SDKs stay out.

pub mod client;
pub mod dual_transport;
pub mod http_conn;

pub use client::{http_call, http_call_with_body, HttpCall};
