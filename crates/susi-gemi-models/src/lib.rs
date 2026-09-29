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

pub use susi_core;

pub use susi_vendor_models::{
    cloud, cloud_manage, eco_schema, frontier, hf_discovery, local_ecosystem, open_weight,
    openrouter,
};
pub mod coding_models;
pub(crate) mod download;
pub mod hardware;
pub mod intent;
mod lifecycle;
pub mod model_cache;

pub use lifecycle::*;
