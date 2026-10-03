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
    cloud, cloud_budget, cloud_contracts, cloud_credentials, cloud_eligibility, cloud_inference,
    cloud_manage, eco_conflicts, eco_consistency, eco_coverage, eco_engine_capability_map,
    eco_matrix, eco_profile, eco_provenance, eco_relations, eco_schema, eco_store, eco_taxonomy,
    frontier, hf_discovery, local_ecosystem, open_weight, openrouter,
};
pub mod coding_models;
pub(crate) mod download;
pub mod gpu_telemetry;
pub mod hardware;
pub mod intent;
mod lifecycle;
pub mod model_cache;
pub mod zc_first_model;
pub mod zc_ladder_edges;
pub mod zc_ladder_live;
pub mod zc_limits_derived;

pub use lifecycle::*;
