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

//! GEMI — universal inference & model substrate.
//!
//! # Two-tier layout
//!
//! | Tier | Crate | Responsibility |
//! |------|-------|----------------|
//! | **Models** | [`susi_gemi_models`] / [`models`] | Select & provision (lifecycle, ladder, coding catalog) |
//! | **Engines** | [`engines`] | Run inference (backends, HTTP/MCP providers, router) |
//!
//! Engines depends on models. Models must not depend on this crate.
//!
//! Flat module paths (`engine`, `http_provider`, `hardware`, …) remain as
//! compatibility re-exports for existing call sites.

extern crate self as susi_gemi_models;

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod engines;

// Models tier is compiled from its canonical source tree without a Cargo edge.
pub mod models;

// Cross-cutting surfaces that use both tiers
pub mod plane_handler;

pub mod benchmark;
pub mod coding_models_ext;
pub mod eval;
pub mod frontier_ext;
pub mod open_weight_ext;
pub mod openrouter_ext;
pub mod pulse;
pub use crate::susi_core::telemetry;

// ── Flat compatibility re-exports (do not remove without a migration) ─────
pub use engines::runtime as engine;
pub(crate) use engines::token_stream;
pub use engines::{
    alpha, candle_provider, http_provider, mcp_provider, qwen2_split, reflex, routing, speculative,
};

pub use models::cloud;
pub(crate) use models::download;
pub use models::model_cache;
pub use models::ModelManager;
pub use models::{
    coding_models, frontier, hardware, hf_discovery, intent, open_weight, openrouter,
};
