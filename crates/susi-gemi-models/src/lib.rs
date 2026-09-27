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

//! GEMI **models** tier: catalog, ladder, download, hardware fit, coding-model control plane.
//!
//! This crate decides *which* weights/endpoints are available. It must not depend on
//! `susi-gemi` engines (Candle forward graphs, HTTP providers, routers). Cloud
//! key/endpoint helpers live here for metadata and CLI key storage only.
//!
//! Prefer `susi_gemi::models::…` (re-exported from the engines crate) for call sites
//! that already depend on `susi-gemi`. Depend on this crate directly when only
//! selection/provisioning is needed.

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

// Vendored `susi-core` contract: byte-identical to
// `crates/susi-core/vendor_template/susi_core/`. dead_code +
// collapsible_if: the canonical tree is edition-2021-shaped while this
// crate is 2024 — forking lint fixes per consumer would break the
// byte-identical invariant. rustfmt::skip for the same reason.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
#[path = "../../susi-core/src/embedded.rs"]
pub mod susi_core;

pub mod cloud;
pub mod coding_models;
pub(crate) mod download;
pub mod frontier;
pub mod hardware;
pub mod hf_discovery;
pub mod intent;
mod lifecycle;
pub mod model_cache;
pub mod open_weight;
pub mod openrouter;

pub use lifecycle::*;
