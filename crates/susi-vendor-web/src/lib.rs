#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Public web API integrations (Open-Meteo weather, DuckDuckGo instant
//! answers). Zero-config, no vendor keys; outbound HTTP goes through
//! `susi-http-transport` and every request is egress-gated.

pub use susi_core;
pub use susi_error;

pub mod live_search;
