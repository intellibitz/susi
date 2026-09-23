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

//! GAWD **swarm** tier: AMA / AMAS orchestration, MissionDag, cloud recovery.
//!
//! Depends on [`susi_gawd_agents`] only. Must not import `ra2a` or `susi-gawd`.

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored-error boundary: susi-core/susi-gawd-agents APIs return their own
// vendored `EaiError`; these conversions preserve the error kind via
// `rewrap` so `?` keeps working across the vendored boundary.
impl From<susi_core::susi_error::EaiError> for susi_error::EaiError {
    fn from(e: susi_core::susi_error::EaiError) -> Self {
        susi_error::rewrap(e.kind_name(), e.to_string())
    }
}

impl From<susi_gawd_agents::susi_error::EaiError> for susi_error::EaiError {
    fn from(e: susi_gawd_agents::susi_error::EaiError) -> Self {
        susi_error::rewrap(e.kind_name(), e.to_string())
    }
}

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

pub mod ama;
pub mod amas;
pub(crate) mod cloud_recovery;
pub mod dag;
pub mod host_hooks;
pub mod peer_registry;

pub use ama::SusiMasterAgent;
pub use dag::{MissionDag, SwarmDag};

/// Wire the MissionDag post-swarm hook into the agents leaf.
///
/// Call once from the host (`susi-gawd`) at load — idempotent first-wins.
pub fn init() {
    susi_gawd_agents::dag_hooks::init(dag::dispatch_mission_dag);
}
