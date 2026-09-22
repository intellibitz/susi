//! GEMI **models** tier: catalog, ladder, download, hardware fit, coding-model control plane.
//!
//! This crate decides *which* weights/endpoints are available. It must not depend on
//! `susi-gemi` engines (Candle forward graphs, HTTP providers, routers). Cloud
//! key/endpoint helpers live here for metadata and CLI key storage only.
//!
//! Prefer `susi_gemi::models::…` (re-exported from the engines crate) for call sites
//! that already depend on `susi-gemi`. Depend on this crate directly when only
//! selection/provisioning is needed.

pub mod catalog_store;
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
