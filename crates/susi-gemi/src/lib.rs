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

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_sandbox;

// Vendored `susi_core` microkernel subset (canonical tree:
// `susi-core/vendor_template/susi_core/`): bus/registry/capture/mac state
// rendezvous with the daemon's real susi_core via `<cache>/bus/<pid>/` +
// substrate files. Allows keep the tree byte-identical across consumers:
// dead_code audits the unexercised surface; rustfmt::skip + collapsible_if
// stop edition-2024 style drift against the edition-2021 canonical source.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
pub mod susi_core;

pub mod engines;

// Models tier (physical crate) — preserve `susi_gemi::models::…` paths
pub use susi_gemi_models as models;

// Cross-cutting surfaces that use both tiers
pub mod plane_handler;

pub mod benchmark;
pub mod coding_models_ext;
pub mod eval;
pub mod frontier_ext;
pub mod open_weight_ext;
pub mod openrouter_ext;
pub mod pulse;
pub mod telemetry;

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
