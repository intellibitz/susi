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
//! with a header-read bound, and the one outbound `ureq` agent used by
//! MCP/peer/search callers. Vendor SDKs stay out of this crate.

pub mod client;
pub mod dual_transport;
pub mod http_conn;

pub use client::{http_agent, http_call, HttpCall};
