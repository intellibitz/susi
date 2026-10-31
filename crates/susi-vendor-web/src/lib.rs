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

//! Search-evidence dispatch for admitted tools.
//!
//! The OS does not embed a weather provider or a search engine. Live evidence
//! comes only from a search tool the operator has admitted.

pub use susi_core;
pub use susi_error;

pub mod live_search;
