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

pub mod engines;

// Models tier (physical crate) — preserve `susi_gemi::models::…` paths
pub use susi_gemi_models as models;

// Cross-cutting surfaces that use both tiers
pub mod benchmark;
pub mod coding_models_ext;
pub mod eval;
pub mod frontier_ext;
pub mod open_weight_ext;
pub mod openrouter_ext;
pub mod pulse;

// ── Flat compatibility re-exports (do not remove without a migration) ─────
pub use engines::runtime as engine;
pub(crate) use engines::token_stream;
pub use engines::{
    alpha, candle_provider, http_provider, mcp_provider, qwen2_split, reasoning, reflex, routing,
    speculative,
};

pub use models::model_cache;
pub use models::{
    coding_models, frontier, hardware, hf_discovery, intent, open_weight, openrouter,
};
